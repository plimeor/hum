package app

import (
	"fmt"
	"os"
	"strings"

	"github.com/plimeor/hum/internal/config"
	"github.com/plimeor/hum/internal/envx"
	"github.com/plimeor/hum/internal/plist"
)

// Severity of a doctor finding.
const (
	SevOK    = "ok"
	SevWarn  = "warn"
	SevError = "error"
)

// Finding is one doctor check result.
type Finding struct {
	Severity string
	Subject  string // service name or "global"
	Message  string
}

// oversizeFactor flags logs this many times larger than max_size: the
// hourly rotation should have kept them below it.
const oversizeFactor = 2

// Doctor checks for common problems.
func (a *App) Doctor() ([]Finding, error) {
	var fs []Finding
	add := func(sev, subj, format string, args ...any) {
		fs = append(fs, Finding{sev, subj, fmt.Sprintf(format, args...)})
	}

	g, gerr := config.LoadGlobal(a.Paths)
	if gerr != nil {
		add(SevError, "global", "%v", gerr)
		g = &config.Global{}
	} else if g.Env["PATH"] == "" {
		add(SevWarn, "global", "no PATH in %s [env]; services get no PATH under launchd", a.Paths.GlobalFile())
	} else {
		add(SevOK, "global", "config ok")
	}
	globalPath := g.Env["PATH"]

	names, err := a.Paths.ServiceNames()
	if err != nil {
		return nil, err
	}
	plistNames, err := a.Paths.PlistNames()
	if err != nil {
		return nil, err
	}
	hasCfg := map[string]bool{}
	for _, n := range names {
		hasCfg[n] = true
	}

	checkProgram := func(subj, plistPath string) {
		data, err := os.ReadFile(plistPath)
		if err != nil {
			return
		}
		prog, ok := plist.Program(data)
		if !ok {
			add(SevError, subj, "cannot read ProgramArguments from %s", plistPath)
			return
		}
		info, err := os.Stat(prog)
		switch {
		case err != nil:
			add(SevError, subj, "plist runs %s, which does not exist; run `hum apply`", prog)
		case info.IsDir() || info.Mode()&0o111 == 0:
			add(SevError, subj, "plist runs %s, which is not executable", prog)
		case strings.Contains(prog, "/Cellar/"):
			add(SevWarn, subj, "plist runs versioned path %s; it breaks on upgrade (put hum on PATH and run `hum apply`)", prog)
		}
	}

	for _, name := range names {
		if err := config.ValidateName(name); err != nil {
			add(SevError, name, "%v", err)
			continue
		}
		s, err := config.LoadService(a.Paths, name)
		if err != nil {
			add(SevError, name, "%v", err)
			continue
		}
		problems := len(fs)
		if s.Shell == "" {
			if _, err := envx.LookPath(s.Command[0], globalPath); err != nil {
				add(SevError, name, "command %v (global PATH)", err)
			}
		}
		cwd := s.ResolvedCwd(a.Paths.Home)
		if info, err := os.Stat(cwd); err != nil || !info.IsDir() {
			add(SevError, name, "cwd %s does not exist", cwd)
		}
		for _, f := range s.ResolvedEnvFiles(a.Paths.Home) {
			if _, err := os.Stat(f); err != nil {
				add(SevError, name, "env_file %s does not exist; `hum run` will refuse to start", f)
			}
		}
		st, err := a.LD.Status(config.Label(name))
		if err != nil {
			return nil, err
		}
		if out, reason := a.syncReason(s, st); out {
			add(SevWarn, name, "launchd out of sync (%s); run `hum apply`", reason)
		}
		checkProgram(name, a.Paths.PlistFile(name))
		maxSize, _ := config.EffectiveLog(g, s)
		if size := fileSize(a.Paths.LogFile(name)); size > oversizeFactor*maxSize {
			add(SevWarn, name, "log is %s, over %d× max_size; is rotation running?", humanSize(size), oversizeFactor)
		}
		if len(fs) == problems {
			add(SevOK, name, "ok")
		}
	}
	for _, name := range plistNames {
		if !hasCfg[name] {
			add(SevWarn, name, "plist without config; run `hum apply` to unregister")
		}
	}
	if len(names) > 0 {
		rp := a.Paths.PlistFile(config.RotateName)
		if _, err := os.Stat(rp); err != nil {
			add(SevWarn, "_rotate", "log rotation job not installed; run `hum apply`")
		} else {
			checkProgram("_rotate", rp)
		}
	}
	return fs, nil
}

func humanSize(n int64) string {
	switch {
	case n >= 1<<30:
		return fmt.Sprintf("%.1fGB", float64(n)/(1<<30))
	case n >= 1<<20:
		return fmt.Sprintf("%.1fMB", float64(n)/(1<<20))
	case n >= 1<<10:
		return fmt.Sprintf("%.1fKB", float64(n)/(1<<10))
	}
	return fmt.Sprintf("%dB", n)
}
