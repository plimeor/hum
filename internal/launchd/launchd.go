// Package launchd wraps launchctl behind an interface so the rest of hum
// can be tested without macOS.
package launchd

import (
	"bytes"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"regexp"
	"runtime"
	"strconv"
	"strings"
	"time"
)

// Status is what launchd reports about one job.
type Status struct {
	Loaded     bool
	Running    bool
	PID        int
	Path       string // plist path launchd loaded the job from
	LastExit   *int   // nil when the job never exited
	LastSignal string // e.g. "Killed: 9"
}

// Crashed reports whether the job is loaded, not running and last ended
// abnormally.
func (s Status) Crashed() bool {
	return s.Loaded && !s.Running && ((s.LastExit != nil && *s.LastExit != 0) || s.LastSignal != "")
}

// Manager controls launchd jobs in the user's GUI domain.
type Manager interface {
	Status(label string) (Status, error)
	// Disabled returns the labels launchd has marked disabled.
	Disabled() (map[string]bool, error)
	Bootstrap(plistPath string) error
	// Bootout unloads the job and waits until launchd has let go of it.
	// Unloading a job that is not loaded is not an error.
	Bootout(label string) error
	Enable(label string) error
	Disable(label string) error
	// Kickstart starts a loaded job if it is not running.
	Kickstart(label string) error
}

// ErrUnsupported is returned on systems without launchd.
var ErrUnsupported = errors.New("launchd is only available on macOS")

// New returns the launchctl-backed manager, or one that always fails with
// ErrUnsupported off macOS.
func New() Manager {
	if runtime.GOOS != "darwin" {
		return unsupported{}
	}
	return &Launchctl{Domain: fmt.Sprintf("gui/%d", os.Getuid()), BootoutTimeout: 10 * time.Second}
}

type unsupported struct{}

func (unsupported) Status(string) (Status, error)      { return Status{}, ErrUnsupported }
func (unsupported) Disabled() (map[string]bool, error) { return nil, ErrUnsupported }
func (unsupported) Bootstrap(string) error             { return ErrUnsupported }
func (unsupported) Bootout(string) error               { return ErrUnsupported }
func (unsupported) Enable(string) error                { return ErrUnsupported }
func (unsupported) Disable(string) error               { return ErrUnsupported }
func (unsupported) Kickstart(string) error             { return ErrUnsupported }

// Launchctl runs /bin/launchctl.
type Launchctl struct {
	Domain         string // gui/<uid>
	BootoutTimeout time.Duration
}

// launchctl print exits with this code when the service does not exist.
const exitNoSuchService = 113

func (l *Launchctl) run(args ...string) (string, int, error) {
	cmd := exec.Command("launchctl", args...)
	var out, errb bytes.Buffer
	cmd.Stdout = &out
	cmd.Stderr = &errb
	err := cmd.Run()
	code := 0
	if err != nil {
		var ee *exec.ExitError
		if !errors.As(err, &ee) {
			return "", -1, err
		}
		code = ee.ExitCode()
		msg := strings.TrimSpace(errb.String())
		if msg == "" {
			msg = strings.TrimSpace(out.String())
		}
		return out.String(), code, fmt.Errorf("launchctl %s: %s (exit %d)", strings.Join(args, " "), msg, code)
	}
	return out.String(), 0, nil
}

func (l *Launchctl) target(label string) string { return l.Domain + "/" + label }

func (l *Launchctl) Status(label string) (Status, error) {
	out, code, err := l.run("print", l.target(label))
	if code == exitNoSuchService {
		return Status{}, nil
	}
	if err != nil {
		return Status{}, err
	}
	return ParsePrint(out), nil
}

func (l *Launchctl) Disabled() (map[string]bool, error) {
	out, _, err := l.run("print-disabled", l.Domain)
	if err != nil {
		return nil, err
	}
	return ParseDisabled(out), nil
}

func (l *Launchctl) Bootstrap(plistPath string) error {
	_, _, err := l.run("bootstrap", l.Domain, plistPath)
	return err
}

func (l *Launchctl) Bootout(label string) error {
	st, err := l.Status(label)
	if err != nil {
		return err
	}
	if !st.Loaded {
		return nil
	}
	if _, _, err := l.run("bootout", l.target(label)); err != nil {
		// It may have gone away between print and bootout.
		if st, serr := l.Status(label); serr == nil && !st.Loaded {
			return nil
		}
		return err
	}
	// bootout returns before launchd has finished tearing the job down;
	// bootstrap right after it can fail, so wait.
	deadline := time.Now().Add(l.BootoutTimeout)
	for {
		st, err := l.Status(label)
		if err != nil {
			return err
		}
		if !st.Loaded {
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("timed out after %s waiting for %s to unload", l.BootoutTimeout, label)
		}
		time.Sleep(100 * time.Millisecond)
	}
}

func (l *Launchctl) Enable(label string) error {
	_, _, err := l.run("enable", l.target(label))
	return err
}

func (l *Launchctl) Disable(label string) error {
	_, _, err := l.run("disable", l.target(label))
	return err
}

func (l *Launchctl) Kickstart(label string) error {
	_, _, err := l.run("kickstart", l.target(label))
	return err
}

var (
	printLineRe    = regexp.MustCompile(`^\t([a-z][a-z ]*?) = (.*)$`)
	disabledLineRe = regexp.MustCompile(`^\s*"([^"]+)" => (\w+)`)
	leadingIntRe   = regexp.MustCompile(`^-?\d+`)
)

// ParsePrint parses the top-level fields of `launchctl print`.
func ParsePrint(out string) Status {
	st := Status{Loaded: true}
	for _, line := range strings.Split(out, "\n") {
		m := printLineRe.FindStringSubmatch(line)
		if m == nil {
			continue
		}
		key, val := m[1], strings.TrimSpace(m[2])
		switch key {
		case "state":
			st.Running = val == "running"
		case "pid":
			st.PID, _ = strconv.Atoi(val)
		case "path":
			st.Path = val
		case "last exit code":
			// "1", "78: EX_CONFIG" or "(never exited)".
			if d := leadingIntRe.FindString(val); d != "" {
				n, _ := strconv.Atoi(d)
				st.LastExit = &n
			}
		case "last terminating signal":
			st.LastSignal = val
		}
	}
	if st.PID > 0 {
		st.Running = true
	}
	return st
}

// ParseDisabled parses `launchctl print-disabled`. Newer macOS prints
// "=> disabled"/"=> enabled", older "=> true"/"=> false".
func ParseDisabled(out string) map[string]bool {
	res := map[string]bool{}
	for _, line := range strings.Split(out, "\n") {
		m := disabledLineRe.FindStringSubmatch(line)
		if m == nil {
			continue
		}
		if m[2] == "disabled" || m[2] == "true" {
			res[m[1]] = true
		}
	}
	return res
}
