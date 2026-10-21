// Package cli defines hum's commands.
package cli

import (
	"bufio"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"strings"

	"github.com/spf13/cobra"

	"github.com/plimeor/hum/internal/app"
	"github.com/plimeor/hum/internal/config"
)

type state struct {
	app  *app.App
	json bool
	yes  bool
	in   *bufio.Reader
	out  io.Writer
	err  io.Writer
}

// exitError carries an exit code without extra output.
type exitError struct{ code int }

func (e exitError) Error() string { return fmt.Sprintf("exit status %d", e.code) }

// Execute runs hum and returns the process exit code.
func Execute() int {
	st := &state{in: bufio.NewReader(os.Stdin), out: os.Stdout, err: os.Stderr}
	root := newRoot(st)
	if err := root.Execute(); err != nil {
		var ee exitError
		if errors.As(err, &ee) {
			return ee.code
		}
		fmt.Fprintln(os.Stderr, "hum:", err)
		return 1
	}
	return 0
}

func newRoot(st *state) *cobra.Command {
	root := &cobra.Command{
		Use:           "hum",
		Short:         "Run background commands as launchd services, configured by TOML files",
		SilenceUsage:  true,
		SilenceErrors: true,
		PersistentPreRunE: func(cmd *cobra.Command, args []string) error {
			a, err := app.New(st.out, st.err)
			if err != nil {
				return err
			}
			st.app = a
			if skipsGlobalInit(cmd) {
				return nil
			}
			created, err := config.EnsureGlobal(a.Paths, os.Getenv("PATH"))
			if err != nil {
				return err
			}
			if created {
				fmt.Fprintf(st.err, "created %s with the current shell's PATH\n", a.Paths.GlobalFile())
			}
			return nil
		},
	}
	root.PersistentFlags().BoolVar(&st.json, "json", false, "output JSON (ls, show)")
	root.PersistentFlags().BoolVarP(&st.yes, "yes", "y", false, "skip confirmation prompts")
	root.AddGroup(
		&cobra.Group{ID: "config", Title: "Configuration:"},
		&cobra.Group{ID: "life", Title: "Lifecycle:"},
		&cobra.Group{ID: "view", Title: "Inspection:"},
		&cobra.Group{ID: "maint", Title: "Maintenance:"},
	)
	root.AddCommand(
		newNew(st), newEdit(st), newSet(st), newUnset(st), newRm(st),
		newStart(st), newStop(st), newRestart(st),
		newLs(st), newShow(st), newLogs(st),
		newApply(st), newDoctor(st), newRun(st),
	)
	return root
}

// skipsGlobalInit: `hum run` is what launchd invokes, with launchd's short
// PATH, so it must not be the one to capture PATH.
func skipsGlobalInit(cmd *cobra.Command) bool {
	for c := cmd; c != nil; c = c.Parent() {
		switch c.Name() {
		case "run", "completion", cobra.ShellCompRequestCmd, cobra.ShellCompNoDescRequestCmd, "help":
			return true
		}
	}
	return false
}

// targets resolves <name>... / --all.
func (st *state) targets(args []string, all bool) ([]string, error) {
	if all {
		if len(args) > 0 {
			return nil, errors.New("give service names or --all, not both")
		}
		names, err := st.app.Paths.ServiceNames()
		if err != nil {
			return nil, err
		}
		if len(names) == 0 {
			return nil, errors.New("no services configured")
		}
		return names, nil
	}
	if len(args) == 0 {
		return nil, errors.New("give at least one service name, or --all")
	}
	for _, n := range args {
		if err := config.ValidateName(n); err != nil {
			return nil, err
		}
	}
	return args, nil
}

// each runs fn for every name, reporting failures and continuing.
func (st *state) each(names []string, fn func(string) error) error {
	failed := 0
	for _, n := range names {
		if err := fn(n); err != nil {
			fmt.Fprintf(st.err, "hum: %s: %v\n", n, err)
			failed++
		}
	}
	if failed > 0 {
		if len(names) == 1 {
			return exitError{1}
		}
		return fmt.Errorf("%d of %d failed", failed, len(names))
	}
	return nil
}

func (st *state) confirm(prompt string) (bool, error) {
	if st.yes {
		return true, nil
	}
	fmt.Fprintf(st.err, "%s [y/N] ", prompt)
	line, err := st.in.ReadString('\n')
	if err != nil && line == "" {
		fmt.Fprintln(st.err)
		return false, nil
	}
	switch strings.ToLower(strings.TrimSpace(line)) {
	case "y", "yes":
		return true, nil
	}
	return false, nil
}

func (st *state) printJSON(v any) error {
	enc := json.NewEncoder(st.out)
	enc.SetIndent("", "  ")
	return enc.Encode(v)
}

func completeServices(st *state) func(*cobra.Command, []string, string) ([]string, cobra.ShellCompDirective) {
	return func(cmd *cobra.Command, args []string, _ string) ([]string, cobra.ShellCompDirective) {
		p, err := config.DefaultPaths()
		if err != nil {
			return nil, cobra.ShellCompDirectiveNoFileComp
		}
		names, _ := p.ServiceNames()
		return names, cobra.ShellCompDirectiveNoFileComp
	}
}
