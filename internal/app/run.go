package app

import (
	"fmt"
	"time"

	"github.com/plimeor/hum/internal/config"
	"github.com/plimeor/hum/internal/envx"
	"github.com/plimeor/hum/internal/logfile"
)

// Exec is everything `hum run` needs to replace itself with the service.
type Exec struct {
	Path string   // resolved program
	Argv []string // argv[0] as configured
	Env  []string
	Dir  string
}

// PrepareRun loads the service, rotates its log if oversized and
// assembles the environment from scratch. base is the process's base
// variables (envx.Base()).
func (a *App) PrepareRun(name string, base map[string]string) (*Exec, error) {
	s, err := config.LoadService(a.Paths, name)
	if err != nil {
		return nil, err
	}
	g, err := config.LoadGlobal(a.Paths)
	if err != nil {
		return nil, err
	}
	maxSize, keep := config.EffectiveLog(g, s)
	if _, err := logfile.RotateIfNeeded(a.Paths.LogFile(name), maxSize, keep, time.Now()); err != nil {
		a.warnf("log rotation: %v", err)
	}
	env, err := envx.Assemble(base, g, s, a.Paths.Home)
	if err != nil {
		return nil, err
	}
	for _, u := range env.Undefined {
		a.warnf("%s:%d: ${%s} is undefined, expanded to \"\"", u.File, u.Line, u.Name)
	}
	argv := s.Argv()
	path, err := envx.LookPath(argv[0], env.Vars["PATH"])
	if err != nil {
		return nil, fmt.Errorf("%s: %w", name, err)
	}
	return &Exec{Path: path, Argv: argv, Env: env.Environ(), Dir: s.ResolvedCwd(a.Paths.Home)}, nil
}
