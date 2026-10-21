package cli

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/signal"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"text/tabwriter"

	"github.com/spf13/cobra"

	"github.com/plimeor/hum/internal/app"
	"github.com/plimeor/hum/internal/config"
	"github.com/plimeor/hum/internal/envx"
	"github.com/plimeor/hum/internal/logfile"
)

func lifecycle(st *state, use, short string, fn func(*app.App, string) error) *cobra.Command {
	var all bool
	cmd := &cobra.Command{
		Use:               use + " <name>...",
		Short:             short,
		GroupID:           "life",
		ValidArgsFunction: completeServices(st),
		RunE: func(cmd *cobra.Command, args []string) error {
			names, err := st.targets(args, all)
			if err != nil {
				return err
			}
			return st.each(names, func(n string) error { return fn(st.app, n) })
		},
	}
	cmd.Flags().BoolVar(&all, "all", false, "all services")
	return cmd
}

func newStart(st *state) *cobra.Command {
	return lifecycle(st, "start", "Start services (and clear a previous stop)", (*app.App).Start)
}

func newStop(st *state) *cobra.Command {
	return lifecycle(st, "stop", "Stop services and keep them stopped after login", (*app.App).Stop)
}

func newRestart(st *state) *cobra.Command {
	return lifecycle(st, "restart", "Regenerate plists and restart services", (*app.App).Restart)
}

func newLs(st *state) *cobra.Command {
	return &cobra.Command{
		Use:     "ls",
		Short:   "List services and their state",
		GroupID: "view",
		Args:    cobra.NoArgs,
		RunE: func(cmd *cobra.Command, args []string) error {
			rows, err := st.app.States()
			if err != nil {
				return err
			}
			if st.json {
				if rows == nil {
					rows = []app.ServiceState{}
				}
				return st.printJSON(rows)
			}
			if len(rows) == 0 {
				fmt.Fprintln(st.out, "no services; create one with `hum new <name> -- <cmd>`")
				return nil
			}
			tw := tabwriter.NewWriter(st.out, 0, 4, 2, ' ', 0)
			fmt.Fprintln(tw, "NAME\tSTATE\tPID\tLAST EXIT\tCOMMAND")
			outOfSync := false
			for _, r := range rows {
				name := r.Name
				if r.OutOfSync {
					name += " *"
					outOfSync = true
				}
				pid := "-"
				if r.PID > 0 {
					pid = strconv.Itoa(r.PID)
				}
				command := r.Command
				if r.Error != "" {
					command = "invalid config: " + r.Error
				} else if r.Reason == "plist without config" {
					command = "(no config)"
				}
				fmt.Fprintf(tw, "%s\t%s\t%s\t%s\t%s\n", name, r.State, pid, lastExit(r.LastExit, r.LastSignal), command)
			}
			if err := tw.Flush(); err != nil {
				return err
			}
			if outOfSync {
				fmt.Fprintln(st.out, "\n* launchd does not match the config; run `hum apply`")
			}
			return nil
		},
	}
}

func lastExit(code *int, signal string) string {
	if signal != "" {
		return signal
	}
	if code == nil {
		return "-"
	}
	return strconv.Itoa(*code)
}

type showEnvVar struct {
	Key    string `json:"key"`
	Value  string `json:"value"`
	Source string `json:"source"`
}

type showUndefined struct {
	File string `json:"file"`
	Line int    `json:"line"`
	Name string `json:"name"`
}

type showOutput struct {
	Name      string          `json:"name"`
	Label     string          `json:"label"`
	Config    string          `json:"config"`
	Plist     string          `json:"plist"`
	Log       string          `json:"log"`
	Command   []string        `json:"command"`
	Shell     bool            `json:"shell"`
	Cwd       string          `json:"cwd"`
	EnvFile   []string        `json:"env_file"`
	Autostart bool            `json:"autostart"`
	Restart   string          `json:"restart"`
	LogMax    int64           `json:"log_max_size"`
	LogKeep   int             `json:"log_keep"`
	State     string          `json:"state"`
	PID       int             `json:"pid,omitempty"`
	LastExit  *int            `json:"last_exit,omitempty"`
	OutOfSync string          `json:"out_of_sync,omitempty"`
	Env       []showEnvVar    `json:"env"`
	EnvError  string          `json:"env_error,omitempty"`
	Undefined []showUndefined `json:"undefined_refs,omitempty"`
}

const masked = "********"

func newShow(st *state) *cobra.Command {
	var reveal bool
	cmd := &cobra.Command{
		Use:               "show <name>",
		Short:             "Show the resolved config, environment and launchd state",
		GroupID:           "view",
		Args:              cobra.ExactArgs(1),
		ValidArgsFunction: completeServices(st),
		RunE: func(cmd *cobra.Command, args []string) error {
			a := st.app
			name := args[0]
			s, err := config.LoadService(a.Paths, name)
			if err != nil {
				return err
			}
			g, err := config.LoadGlobal(a.Paths)
			if err != nil {
				return err
			}
			label := config.Label(name)
			ls, err := a.LD.Status(label)
			if err != nil {
				return err
			}
			disabled, err := a.LD.Disabled()
			if err != nil {
				return err
			}
			maxSize, keep := config.EffectiveLog(g, s)
			out := showOutput{
				Name: name, Label: label,
				Config: a.Paths.ServiceFile(name), Plist: a.Paths.PlistFile(name), Log: a.Paths.LogFile(name),
				Command: s.Argv(), Shell: s.Shell != "",
				Cwd: s.ResolvedCwd(a.Paths.Home), EnvFile: s.ResolvedEnvFiles(a.Paths.Home),
				Autostart: s.Autostart, Restart: s.Restart, LogMax: maxSize, LogKeep: keep,
				State: app.StateOf(ls, disabled[label]), PID: ls.PID, LastExit: ls.LastExit,
			}
			exists, current := a.PlistCurrent(s)
			if !exists {
				out.OutOfSync = "plist missing"
			} else if !current {
				out.OutOfSync = "plist outdated"
			}
			env, eerr := envx.Assemble(envx.Base(), g, s, a.Paths.Home)
			if eerr != nil {
				out.EnvError = eerr.Error()
			}
			for _, k := range env.Keys() {
				v := env.Vars[k]
				if !reveal {
					v = masked
				}
				out.Env = append(out.Env, showEnvVar{k, v, env.Sources[k]})
			}
			for _, u := range env.Undefined {
				out.Undefined = append(out.Undefined, showUndefined{u.File, u.Line, u.Name})
			}
			if st.json {
				return st.printJSON(out)
			}
			printShow(st, out, s.CommandString(), ls.LastSignal)
			return nil
		},
	}
	cmd.Flags().BoolVar(&reveal, "reveal", false, "show environment variable values")
	return cmd
}

func printShow(st *state, o showOutput, command, signal string) {
	w := tabwriter.NewWriter(st.out, 0, 4, 2, ' ', 0)
	kv := func(k, v string) { fmt.Fprintf(w, "%s\t%s\n", k, v) }
	kv("name", o.Name)
	state := o.State
	if o.PID > 0 {
		state += fmt.Sprintf(" (pid %d)", o.PID)
	}
	kv("state", state)
	kv("last exit", lastExit(o.LastExit, signal))
	kv("command", command)
	kv("cwd", o.Cwd)
	if len(o.EnvFile) > 0 {
		kv("env_file", strings.Join(o.EnvFile, ", "))
	}
	kv("autostart", strconv.FormatBool(o.Autostart))
	kv("restart", o.Restart)
	kv("log rotate", fmt.Sprintf("max_size %d bytes, keep %d", o.LogMax, o.LogKeep))
	kv("config", o.Config)
	kv("plist", o.Plist)
	kv("log", o.Log)
	if o.OutOfSync != "" {
		kv("launchd", o.OutOfSync+"; run `hum apply` or `hum restart "+o.Name+"`")
	}
	w.Flush()

	fmt.Fprintln(st.out, "\nenvironment:")
	w = tabwriter.NewWriter(st.out, 0, 4, 2, ' ', 0)
	for _, e := range o.Env {
		fmt.Fprintf(w, "  %s\t%s\t%s\n", e.Key, e.Value, e.Source)
	}
	w.Flush()
	if o.EnvError != "" {
		fmt.Fprintf(st.out, "  error: %s (hum run would refuse to start)\n", o.EnvError)
	}
	for _, u := range o.Undefined {
		fmt.Fprintf(st.out, "  warning: %s:%d references undefined ${%s} (expands to \"\")\n", u.File, u.Line, u.Name)
	}
}

func newLogs(st *state) *cobra.Command {
	var (
		all, follow, rotate, ifOversize bool
		lines                           int
	)
	cmd := &cobra.Command{
		Use:               "logs <name>...",
		Short:             "Show, follow or rotate logs",
		GroupID:           "view",
		ValidArgsFunction: completeServices(st),
		RunE: func(cmd *cobra.Command, args []string) error {
			names, err := st.targets(args, all)
			if err != nil {
				return err
			}
			a := st.app
			if rotate {
				g, err := config.LoadGlobal(a.Paths)
				if err != nil {
					return err
				}
				return st.each(names, func(n string) error {
					s, err := config.LoadService(a.Paths, n)
					if err != nil {
						s = nil // rotate with global settings anyway
					}
					maxSize, keep := config.EffectiveLog(g, s)
					path := a.Paths.LogFile(n)
					if ifOversize {
						rotated, err := logfile.RotateIfNeeded(path, maxSize, keep)
						if rotated {
							fmt.Fprintf(st.out, "%s: rotated\n", n)
						}
						return err
					}
					if err := logfile.Rotate(path, keep); err != nil {
						return err
					}
					fmt.Fprintf(st.out, "%s: rotated\n", n)
					return nil
				})
			}
			prefix := len(names) > 1 || all
			var mu sync.Mutex
			emit := func(n, line string) {
				mu.Lock()
				defer mu.Unlock()
				if prefix {
					fmt.Fprintf(st.out, "[%s] %s\n", n, line)
				} else {
					fmt.Fprintln(st.out, line)
				}
			}
			offsets := make([]int64, len(names))
			for i, n := range names {
				ls, off, err := logfile.Tail(a.Paths.LogFile(n), lines)
				if err != nil {
					return err
				}
				offsets[i] = off
				for _, l := range ls {
					emit(n, l)
				}
			}
			if !follow {
				return nil
			}
			ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
			defer stop()
			var wg sync.WaitGroup
			errs := make(chan error, len(names))
			for i, n := range names {
				wg.Add(1)
				go func() {
					defer wg.Done()
					errs <- logfile.Follow(ctx, a.Paths.LogFile(n), offsets[i], func(l string) { emit(n, l) })
				}()
			}
			wg.Wait()
			close(errs)
			for err := range errs {
				if err != nil {
					return err
				}
			}
			return nil
		},
	}
	f := cmd.Flags()
	f.BoolVar(&all, "all", false, "all services")
	f.BoolVarP(&follow, "follow", "f", false, "keep printing new lines")
	f.IntVarP(&lines, "lines", "n", 100, "number of lines to show")
	f.BoolVar(&rotate, "rotate", false, "rotate now, regardless of size")
	f.BoolVar(&ifOversize, "if-oversize", false, "with --rotate: only rotate logs over max_size (used by the hourly job)")
	_ = f.MarkHidden("if-oversize")
	return cmd
}

func newApply(st *state) *cobra.Command {
	var dryRun bool
	cmd := &cobra.Command{
		Use:     "apply",
		Short:   "Align launchd with the config directory",
		GroupID: "maint",
		Args:    cobra.NoArgs,
		RunE: func(cmd *cobra.Command, args []string) error {
			actions, problems, err := st.app.Plan()
			if err != nil {
				return err
			}
			for _, p := range problems {
				fmt.Fprintf(st.err, "skipped: %v\n", p)
			}
			if dryRun {
				if len(actions) == 0 {
					fmt.Fprintln(st.out, "nothing to do")
				}
				for _, act := range actions {
					fmt.Fprintf(st.out, "would %s %s\n", act.Kind, act.Name)
				}
				return nil
			}
			if len(actions) == 0 {
				fmt.Fprintln(st.out, "launchd is in sync")
			}
			if err := st.app.Apply(actions); err != nil {
				return err
			}
			if len(problems) > 0 {
				return exitError{1}
			}
			return nil
		},
	}
	cmd.Flags().BoolVar(&dryRun, "dry-run", false, "only list the changes")
	return cmd
}

func newDoctor(st *state) *cobra.Command {
	return &cobra.Command{
		Use:     "doctor",
		Short:   "Check for common problems",
		GroupID: "maint",
		Args:    cobra.NoArgs,
		RunE: func(cmd *cobra.Command, args []string) error {
			fs, err := st.app.Doctor()
			if err != nil {
				return err
			}
			errorsFound := 0
			for _, f := range fs {
				mark := "ok   "
				switch f.Severity {
				case app.SevWarn:
					mark = "warn "
				case app.SevError:
					mark = "error"
					errorsFound++
				}
				fmt.Fprintf(st.out, "%s  %-12s %s\n", mark, f.Subject, f.Message)
			}
			if errorsFound > 0 {
				return exitError{1}
			}
			return nil
		},
	}
}

func newRun(st *state) *cobra.Command {
	return &cobra.Command{
		Use:               "run <name>",
		Short:             "Run a service in the foreground (what launchd invokes)",
		GroupID:           "maint",
		Args:              cobra.ExactArgs(1),
		ValidArgsFunction: completeServices(st),
		RunE: func(cmd *cobra.Command, args []string) error {
			ex, err := st.app.PrepareRun(args[0], envx.Base())
			if err != nil {
				var missing *envx.MissingEnvFileError
				if errors.As(err, &missing) {
					// Under launchd stderr is the service log.
					return fmt.Errorf("%s: not starting: %w", args[0], err)
				}
				return err
			}
			if err := os.Chdir(ex.Dir); err != nil {
				return fmt.Errorf("cwd: %w", err)
			}
			return syscall.Exec(ex.Path, ex.Argv, ex.Env)
		},
	}
}
