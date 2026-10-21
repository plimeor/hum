// Package app implements hum's operations on top of config, plist and
// launchd. The CLI layer only parses flags and prints.
package app

import (
	"bytes"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"

	"github.com/plimeor/hum/internal/config"
	"github.com/plimeor/hum/internal/launchd"
	"github.com/plimeor/hum/internal/plist"
)

// RotateInterval is how often the internal rotate job checks log sizes, in
// seconds (pm2-logrotate's workerInterval default).
const RotateInterval = 30

// App ties paths, launchd and output together.
type App struct {
	Paths   config.Paths
	LD      launchd.Manager
	HumPath string // absolute path written into plists
	Out     io.Writer
	Err     io.Writer
}

// New builds an App for the current user.
func New(out, errw io.Writer) (*App, error) {
	p, err := config.DefaultPaths()
	if err != nil {
		return nil, err
	}
	return &App{Paths: p, LD: launchd.New(), HumPath: HumPath(), Out: out, Err: errw}, nil
}

// HumPath picks the path launchd should run. A package manager symlink on
// PATH (e.g. /opt/homebrew/bin/hum) survives upgrades; the resolved binary
// path does not, so the symlink wins when it points at this binary.
func HumPath() string {
	exe, err := os.Executable()
	if err != nil {
		return "hum"
	}
	real, err := filepath.EvalSymlinks(exe)
	if err != nil {
		real = exe
	}
	if lp, err := exec.LookPath("hum"); err == nil {
		if abs, err := filepath.Abs(lp); err == nil {
			if r, err := filepath.EvalSymlinks(abs); err == nil && r == real {
				return abs
			}
		}
	}
	return exe
}

func (a *App) warnf(format string, args ...any) {
	fmt.Fprintf(a.Err, "warning: "+format+"\n", args...)
}

func (a *App) infof(format string, args ...any) {
	fmt.Fprintf(a.Out, format+"\n", args...)
}

func (a *App) jobEnv() map[string]string {
	if a.Paths.HumHome == "" {
		return nil
	}
	return map[string]string{"HUM_HOME": a.Paths.HumHome}
}

// RenderPlist returns the plist a service should have.
func (a *App) RenderPlist(s *config.Service) []byte {
	j := plist.Job{
		Label:            config.Label(s.Name),
		ProgramArguments: []string{a.HumPath, "run", s.Name},
		RunAtLoad:        s.Autostart,
		LogPath:          a.Paths.LogFile(s.Name),
		Env:              a.jobEnv(),
	}
	switch s.Restart {
	case config.RestartAlways:
		j.KeepAlive = plist.KeepAliveAlways
	case config.RestartOnFailure:
		j.KeepAlive = plist.KeepAliveOnFailure
	}
	return plist.Render(j)
}

func (a *App) renderRotatePlist() []byte {
	return plist.Render(plist.Job{
		Label:            config.Label(config.RotateName),
		ProgramArguments: []string{a.HumPath, "logs", "--rotate", "--all", "--if-oversize"},
		StartInterval:    RotateInterval,
		LogPath:          a.Paths.LogFile(config.RotateName),
		Env:              a.jobEnv(),
	})
}

// writePlist writes data to the plist path if it differs. It reports
// whether the file changed.
func (a *App) writePlist(name string, data []byte) (bool, error) {
	path := a.Paths.PlistFile(name)
	if cur, err := os.ReadFile(path); err == nil && bytes.Equal(cur, data) {
		return false, nil
	}
	if err := os.MkdirAll(a.Paths.LogDir, 0o755); err != nil {
		return false, err
	}
	return true, config.WriteFileAtomic(path, data, 0o644)
}

// SyncPlist regenerates a service's plist file without touching launchd.
func (a *App) SyncPlist(s *config.Service) (bool, error) {
	return a.writePlist(s.Name, a.RenderPlist(s))
}

// PlistCurrent reports whether the plist on disk matches the config.
func (a *App) PlistCurrent(s *config.Service) (exists, current bool) {
	cur, err := os.ReadFile(a.Paths.PlistFile(s.Name))
	if err != nil {
		return false, false
	}
	return true, bytes.Equal(cur, a.RenderPlist(s))
}

// Start enables the service, regenerates its plist and starts it. A
// running service is left alone.
func (a *App) Start(name string) error {
	s, err := config.LoadService(a.Paths, name)
	if err != nil {
		return err
	}
	label := config.Label(name)
	st, err := a.LD.Status(label)
	if err != nil {
		return err
	}
	if st.Running {
		a.infof("%s: already running (pid %d)", name, st.PID)
		return nil
	}
	changed, err := a.SyncPlist(s)
	if err != nil {
		return err
	}
	if err := a.LD.Enable(label); err != nil {
		return err
	}
	if st.Loaded && (changed || st.Path != a.Paths.PlistFile(name)) {
		// Loaded with an outdated definition: reload it.
		if err := a.LD.Bootout(label); err != nil {
			return err
		}
		st.Loaded = false
	}
	if !st.Loaded {
		if err := a.LD.Bootstrap(a.Paths.PlistFile(name)); err != nil {
			return err
		}
	}
	// Bootstrap only runs the job when RunAtLoad is set.
	if err := a.LD.Kickstart(label); err != nil {
		return err
	}
	a.infof("%s: started", name)
	return a.EnsureRotateJob()
}

// Stop unloads the service and disables it so it stays stopped after
// login.
func (a *App) Stop(name string) error {
	if err := config.ValidateName(name); err != nil {
		return err
	}
	label := config.Label(name)
	if err := a.LD.Bootout(label); err != nil {
		return err
	}
	if err := a.LD.Disable(label); err != nil {
		return err
	}
	a.infof("%s: stopped", name)
	return nil
}

// Restart regenerates the plist, unloads the job and loads it again. A
// stopped (disabled) service is enabled and started.
func (a *App) Restart(name string) error {
	s, err := config.LoadService(a.Paths, name)
	if err != nil {
		return err
	}
	label := config.Label(name)
	if _, err := a.SyncPlist(s); err != nil {
		return err
	}
	if err := a.LD.Bootout(label); err != nil {
		return err
	}
	if err := a.LD.Enable(label); err != nil {
		return err
	}
	if err := a.LD.Bootstrap(a.Paths.PlistFile(name)); err != nil {
		return err
	}
	if err := a.LD.Kickstart(label); err != nil {
		return err
	}
	a.infof("%s: restarted", name)
	return a.EnsureRotateJob()
}

// Remove unloads the service, deletes its plist and, unless keepConfig,
// its config file.
func (a *App) Remove(name string, keepConfig bool) error {
	if err := config.ValidateName(name); err != nil {
		return err
	}
	cfg := a.Paths.ServiceFile(name)
	pl := a.Paths.PlistFile(name)
	if !exists(cfg) && !exists(pl) {
		return fmt.Errorf("service %q not found", name)
	}
	label := config.Label(name)
	if err := a.LD.Bootout(label); err != nil {
		return err
	}
	// Drop the disabled override so a future service with this name
	// does not inherit it.
	if err := a.LD.Enable(label); err != nil {
		return err
	}
	if err := removeIfExists(pl); err != nil {
		return err
	}
	if !keepConfig {
		if err := removeIfExists(cfg); err != nil {
			return err
		}
		a.infof("%s: removed", name)
	} else {
		a.infof("%s: unregistered (config kept at %s)", name, cfg)
	}
	return a.pruneRotateJob()
}

// ApplyConfigChange makes a config edit take effect: the plist is
// regenerated and a running service restarted when restart is true.
func (a *App) ApplyConfigChange(name string, restart bool) error {
	s, err := config.LoadService(a.Paths, name)
	if err != nil {
		return err
	}
	st, err := a.LD.Status(config.Label(name))
	if err != nil {
		return err
	}
	if !st.Running {
		// Keep the file current for the next login; launchd re-reads it
		// on start/restart.
		if exists(a.Paths.PlistFile(name)) {
			_, err := a.SyncPlist(s)
			return err
		}
		return nil
	}
	if !restart {
		a.infof("%s: running; run `hum restart %s` to apply", name, name)
		return nil
	}
	return a.Restart(name)
}

// EnsureRotateJob installs and loads the periodic rotate job.
func (a *App) EnsureRotateJob() error {
	label := config.Label(config.RotateName)
	changed, err := a.writePlist(config.RotateName, a.renderRotatePlist())
	if err != nil {
		return err
	}
	st, err := a.LD.Status(label)
	if err != nil {
		return err
	}
	if st.Loaded && !changed {
		return nil
	}
	if st.Loaded {
		if err := a.LD.Bootout(label); err != nil {
			return err
		}
	}
	if err := a.LD.Enable(label); err != nil {
		return err
	}
	return a.LD.Bootstrap(a.Paths.PlistFile(config.RotateName))
}

// pruneRotateJob removes the rotate job once no service is left.
func (a *App) pruneRotateJob() error {
	names, err := a.Paths.ServiceNames()
	if err != nil || len(names) > 0 {
		return err
	}
	if err := a.LD.Bootout(config.Label(config.RotateName)); err != nil {
		return err
	}
	return removeIfExists(a.Paths.PlistFile(config.RotateName))
}

func exists(p string) bool {
	_, err := os.Stat(p)
	return err == nil
}

func removeIfExists(p string) error {
	if err := os.Remove(p); err != nil && !os.IsNotExist(err) {
		return err
	}
	return nil
}
