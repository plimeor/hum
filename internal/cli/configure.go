package cli

import (
	"bytes"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"

	"github.com/spf13/cobra"

	"github.com/plimeor/hum/internal/config"
	"github.com/plimeor/hum/internal/envx"
	"github.com/plimeor/hum/internal/tomledit"
)

func newNew(st *state) *cobra.Command {
	var (
		cwd, restart       string
		envs, envFiles     []string
		noAutostart, noRun bool
	)
	cmd := &cobra.Command{
		Use:     "new <name> [flags] -- <cmd> [args...]",
		Short:   "Create a service config and start it",
		GroupID: "config",
		Example: "  hum new t3 -- t3 server\n" +
			"  hum new api --cwd ~/work/api --env PORT=4000 --env-file .env --restart on-failure -- bun run start",
		RunE: func(cmd *cobra.Command, args []string) error {
			dash := cmd.ArgsLenAtDash()
			if dash != 1 || len(args) < 2 {
				return errors.New("usage: hum new <name> [flags] -- <cmd> [args...]")
			}
			name, argv := args[0], args[1:]
			a := st.app
			if err := config.ValidateName(name); err != nil {
				return err
			}
			if _, err := os.Stat(a.Paths.ServiceFile(name)); err == nil {
				return fmt.Errorf("service %q already exists; use `hum edit %s` or `hum set %s`", name, name, name)
			}
			if cwd == "" {
				wd, err := os.Getwd()
				if err != nil {
					return err
				}
				cwd = wd
			} else {
				abs, err := filepath.Abs(config.ExpandPath(cwd, a.Paths.Home))
				if err != nil {
					return err
				}
				cwd = abs
			}
			s := &config.Service{
				Name:      name,
				Command:   argv,
				Cwd:       config.ContractPath(cwd, a.Paths.Home),
				EnvFile:   envFiles,
				Autostart: !noAutostart,
				Restart:   restart,
				Env:       map[string]string{},
			}
			for _, kv := range envs {
				k, v, ok := strings.Cut(kv, "=")
				if !ok {
					return fmt.Errorf("--env %q: expected KEY=VALUE", kv)
				}
				if err := config.ValidateEnvKey(k); err != nil {
					return err
				}
				s.Env[k] = v
			}
			data := config.RenderService(s)
			if _, err := config.ParseService(name, data); err != nil {
				return err
			}
			g, err := config.LoadGlobal(a.Paths)
			if err != nil {
				return err
			}
			if _, err := envx.LookPath(argv[0], g.Env["PATH"]); err != nil {
				fmt.Fprintf(st.err, "warning: %v (global PATH); writing it anyway\n", err)
			}
			warnMissingEnvFiles(st, s)
			if err := config.WriteFileAtomic(a.Paths.ServiceFile(name), data, 0o644); err != nil {
				return err
			}
			fmt.Fprintf(st.out, "created %s\n", a.Paths.ServiceFile(name))
			if noRun {
				_, err := a.SyncPlist(s)
				return err
			}
			return a.Start(name)
		},
	}
	f := cmd.Flags()
	f.StringVar(&cwd, "cwd", "", "working directory (default: current directory)")
	f.StringArrayVar(&envs, "env", nil, "environment variable KEY=VALUE (repeatable)")
	f.StringArrayVar(&envFiles, "env-file", nil, "env file, relative to --cwd (repeatable)")
	f.StringVar(&restart, "restart", config.RestartAlways, "restart policy: always | on-failure | never")
	f.BoolVar(&noAutostart, "no-autostart", false, "do not start at login")
	f.BoolVar(&noRun, "no-start", false, "write the config without starting the service")
	return cmd
}

func warnMissingEnvFiles(st *state, s *config.Service) {
	for _, f := range s.ResolvedEnvFiles(st.app.Paths.Home) {
		if _, err := os.Stat(f); err != nil {
			fmt.Fprintf(st.err, "warning: env_file %s does not exist; `hum run` will refuse to start until it does\n", f)
		}
	}
}

func newEdit(st *state) *cobra.Command {
	var global, noRestart bool
	cmd := &cobra.Command{
		Use:               "edit <name> | --global",
		Short:             "Edit a config in $EDITOR, validate it, apply it",
		GroupID:           "config",
		ValidArgsFunction: completeServices(st),
		RunE: func(cmd *cobra.Command, args []string) error {
			a := st.app
			var path string
			var validate func([]byte) error
			switch {
			case global && len(args) == 0:
				path = a.Paths.GlobalFile()
				validate = func(b []byte) error { _, err := config.ParseGlobal(b); return err }
			case !global && len(args) == 1:
				name := args[0]
				if err := config.ValidateName(name); err != nil {
					return err
				}
				path = a.Paths.ServiceFile(name)
				validate = func(b []byte) error { _, err := config.ParseService(name, b); return err }
			default:
				return errors.New("usage: hum edit <name> | hum edit --global")
			}
			orig, err := os.ReadFile(path)
			if err != nil {
				if os.IsNotExist(err) && !global {
					return fmt.Errorf("service %q not found; create it with `hum new`", args[0])
				}
				return err
			}
			edited, err := st.editLoop(path, orig, validate)
			if err != nil || edited == nil {
				return err
			}
			if err := config.WriteFileAtomic(path, edited, 0o644); err != nil {
				return err
			}
			fmt.Fprintf(st.out, "saved %s\n", path)
			if global {
				return st.applyGlobalChange(!noRestart)
			}
			s, _ := config.ParseService(args[0], edited)
			warnMissingEnvFiles(st, s)
			return a.ApplyConfigChange(args[0], !noRestart)
		},
	}
	cmd.Flags().BoolVar(&global, "global", false, "edit config.toml")
	cmd.Flags().BoolVar(&noRestart, "no-restart", false, "do not restart running services")
	return cmd
}

// editLoop opens a temp copy in the editor until it validates or the user
// gives up. It returns nil when nothing changed or the edit was abandoned.
func (st *state) editLoop(path string, orig []byte, validate func([]byte) error) ([]byte, error) {
	tmp, err := os.CreateTemp("", "hum-*-"+filepath.Base(path))
	if err != nil {
		return nil, err
	}
	defer os.Remove(tmp.Name())
	if _, err := tmp.Write(orig); err != nil {
		tmp.Close()
		return nil, err
	}
	if err := tmp.Close(); err != nil {
		return nil, err
	}
	for {
		if err := runEditor(tmp.Name()); err != nil {
			return nil, err
		}
		data, err := os.ReadFile(tmp.Name())
		if err != nil {
			return nil, err
		}
		if bytes.Equal(data, orig) {
			fmt.Fprintln(st.out, "no changes")
			return nil, nil
		}
		verr := validate(data)
		if verr == nil {
			return data, nil
		}
		fmt.Fprintf(st.err, "invalid config: %v\n", verr)
		again := true
		if !st.yes {
			fmt.Fprint(st.err, "edit again? [Y/n] ")
			line, err := st.in.ReadString('\n')
			ans := strings.ToLower(strings.TrimSpace(line))
			again = err == nil && (ans == "" || ans == "y" || ans == "yes")
		}
		if !again {
			fmt.Fprintf(st.err, "discarded changes; %s is unchanged\n", path)
			return nil, exitError{1}
		}
	}
}

func runEditor(file string) error {
	editor := os.Getenv("VISUAL")
	if editor == "" {
		editor = os.Getenv("EDITOR")
	}
	if editor == "" {
		editor = "vi"
	}
	// Through sh so that editors with arguments ("code -w") work.
	c := exec.Command("/bin/sh", "-c", editor+` "$@"`, "editor", file)
	c.Stdin, c.Stdout, c.Stderr = os.Stdin, os.Stdout, os.Stderr
	if err := c.Run(); err != nil {
		return fmt.Errorf("editor %q: %w", editor, err)
	}
	return nil
}

// applyGlobalChange restarts every running service, since global [env]
// and [log] affect all of them.
func (st *state) applyGlobalChange(restart bool) error {
	names, err := st.app.Paths.ServiceNames()
	if err != nil {
		return err
	}
	return st.each(names, func(n string) error { return st.app.ApplyConfigChange(n, restart) })
}

// setKeys are the keys `hum set` accepts besides env.*.
var setKeys = map[string]bool{
	"command": true, "cwd": true, "env_file": true, "autostart": true, "restart": true,
	"log.max_size": true, "log.keep": true,
}

func keyPath(key string) ([]string, error) {
	if strings.HasPrefix(key, "env.") {
		k := strings.TrimPrefix(key, "env.")
		if err := config.ValidateEnvKey(k); err != nil {
			return nil, err
		}
		return []string{"env", k}, nil
	}
	if !setKeys[key] {
		return nil, fmt.Errorf("unknown key %q (known: command, cwd, env_file, autostart, restart, env.<NAME>, log.max_size, log.keep)", key)
	}
	return strings.Split(key, "."), nil
}

func literalFor(key, value string) (string, error) {
	switch key {
	case "autostart":
		b, err := strconv.ParseBool(value)
		if err != nil {
			return "", fmt.Errorf("autostart: expected true or false, got %q", value)
		}
		return strconv.FormatBool(b), nil
	case "log.keep":
		n, err := strconv.Atoi(value)
		if err != nil {
			return "", fmt.Errorf("log.keep: expected an integer, got %q", value)
		}
		return strconv.Itoa(n), nil
	case "env_file":
		var items []string
		for _, it := range strings.Split(value, ",") {
			if it = strings.TrimSpace(it); it != "" {
				items = append(items, it)
			}
		}
		return config.QuoteArray(items), nil
	}
	return config.Quote(value), nil
}

func newSet(st *state) *cobra.Command {
	var noRestart bool
	cmd := &cobra.Command{
		Use:     "set <name> <key>=<value>... [command -- <cmd> [args...]]",
		Short:   "Change config fields from the command line",
		GroupID: "config",
		Example: "  hum set t3 env.PORT=4000 restart=on-failure\n" +
			"  hum set t3 env_file=.env,.env.local\n" +
			"  hum set t3 command -- t3 server --port 1",
		ValidArgsFunction: completeServices(st),
		RunE: func(cmd *cobra.Command, args []string) error {
			if len(args) < 2 {
				return errors.New("usage: hum set <name> <key>=<value>...")
			}
			name := args[0]
			before, after := args[1:], []string(nil)
			if dash := cmd.ArgsLenAtDash(); dash >= 0 {
				if dash < 1 {
					return errors.New("usage: hum set <name> command -- <cmd> [args...]")
				}
				before, after = args[1:dash], args[dash:]
			}
			return st.editService(name, noRestart, func(doc *tomledit.Doc) error {
				commandFromDash := false
				for _, kv := range before {
					if kv == "command" {
						if len(after) == 0 {
							return errors.New("command: give the new command after --, e.g. `hum set t3 command -- t3 server`")
						}
						if err := doc.Set([]string{"command"}, config.QuoteArray(after)); err != nil {
							return err
						}
						commandFromDash = true
						continue
					}
					k, v, ok := strings.Cut(kv, "=")
					if !ok {
						return fmt.Errorf("%q: expected key=value", kv)
					}
					path, err := keyPath(k)
					if err != nil {
						return err
					}
					lit, err := literalFor(k, v)
					if err != nil {
						return err
					}
					if err := doc.Set(path, lit); err != nil {
						return err
					}
				}
				if len(after) > 0 && !commandFromDash {
					return errors.New("arguments after -- are only used with the `command` key")
				}
				return nil
			})
		},
	}
	cmd.Flags().BoolVar(&noRestart, "no-restart", false, "do not restart the service if it is running")
	return cmd
}

func newUnset(st *state) *cobra.Command {
	var noRestart bool
	cmd := &cobra.Command{
		Use:               "unset <name> <key>...",
		Short:             "Remove config fields",
		GroupID:           "config",
		Example:           "  hum unset t3 env.DEBUG",
		ValidArgsFunction: completeServices(st),
		RunE: func(cmd *cobra.Command, args []string) error {
			if len(args) < 2 {
				return errors.New("usage: hum unset <name> <key>...")
			}
			return st.editService(args[0], noRestart, func(doc *tomledit.Doc) error {
				for _, k := range args[1:] {
					if k == "command" {
						return errors.New("command is required and cannot be unset")
					}
					path, err := keyPath(k)
					if err != nil {
						return err
					}
					found, err := doc.Unset(path)
					if err != nil {
						return err
					}
					if !found {
						fmt.Fprintf(st.err, "warning: %s is not set\n", k)
					}
				}
				return nil
			})
		},
	}
	cmd.Flags().BoolVar(&noRestart, "no-restart", false, "do not restart the service if it is running")
	return cmd
}

// editService applies fn to the service file, validates the result and
// writes it back, then applies the change like `hum edit`.
func (st *state) editService(name string, noRestart bool, fn func(*tomledit.Doc) error) error {
	a := st.app
	if err := config.ValidateName(name); err != nil {
		return err
	}
	path := a.Paths.ServiceFile(name)
	orig, err := os.ReadFile(path)
	if os.IsNotExist(err) {
		return fmt.Errorf("service %q not found", name)
	}
	if err != nil {
		return err
	}
	doc := tomledit.New(orig)
	if err := fn(doc); err != nil {
		return err
	}
	out := doc.Bytes()
	if bytes.Equal(out, orig) {
		fmt.Fprintln(st.out, "no changes")
		return nil
	}
	s, err := config.ParseService(name, out)
	if err != nil {
		return fmt.Errorf("resulting config is invalid, %s left unchanged: %w", path, err)
	}
	if err := config.WriteFileAtomic(path, out, 0o644); err != nil {
		return err
	}
	fmt.Fprintf(st.out, "saved %s\n", path)
	warnMissingEnvFiles(st, s)
	return a.ApplyConfigChange(name, !noRestart)
}

func newRm(st *state) *cobra.Command {
	var all, keep bool
	cmd := &cobra.Command{
		Use:               "rm <name>...",
		Short:             "Stop and delete services",
		GroupID:           "config",
		ValidArgsFunction: completeServices(st),
		RunE: func(cmd *cobra.Command, args []string) error {
			names, err := st.targets(args, all)
			if err != nil {
				return err
			}
			what := "stop and delete"
			if keep {
				what = "stop and unregister"
			}
			ok, err := st.confirm(fmt.Sprintf("%s %s?", what, strings.Join(names, ", ")))
			if err != nil {
				return err
			}
			if !ok {
				return errors.New("aborted")
			}
			return st.each(names, func(n string) error { return st.app.Remove(n, keep) })
		},
	}
	cmd.Flags().BoolVar(&all, "all", false, "all services")
	cmd.Flags().BoolVar(&keep, "keep-config", false, "unregister but keep the config file")
	return cmd
}
