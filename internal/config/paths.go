// Package config loads, validates and writes hum's TOML configuration and
// knows where every file hum touches lives.
package config

import (
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strings"
)

// LabelPrefix prefixes every launchd label hum owns.
const LabelPrefix = "hum."

// RotateName is the internal job that rotates logs. Service names cannot
// start with "_", so it never collides with a user service.
const RotateName = "_rotate"

var nameRe = regexp.MustCompile(`^[a-z0-9][a-z0-9_-]*$`)

// ValidateName reports whether name can be used as a service name.
func ValidateName(name string) error {
	if !nameRe.MatchString(name) {
		return fmt.Errorf("invalid service name %q: must match [a-z0-9][a-z0-9_-]*", name)
	}
	return nil
}

// Label returns the launchd label for a service name.
func Label(name string) string { return LabelPrefix + name }

// Paths holds every location hum reads or writes.
type Paths struct {
	Home         string // user's home directory
	ConfigDir    string // ~/.config/hum, or $HUM_HOME
	LaunchAgents string // ~/Library/LaunchAgents
	LogDir       string // ~/Library/Logs/hum
	// HumHome is the value of $HUM_HOME when set; it is passed on to
	// launchd jobs so `hum run` reads the same directory.
	HumHome string
}

// DefaultPaths resolves paths from the environment.
func DefaultPaths() (Paths, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return Paths{}, err
	}
	p := Paths{
		Home:         home,
		ConfigDir:    filepath.Join(home, ".config", "hum"),
		LaunchAgents: filepath.Join(home, "Library", "LaunchAgents"),
		LogDir:       filepath.Join(home, "Library", "Logs", "hum"),
	}
	if h := os.Getenv("HUM_HOME"); h != "" {
		abs, err := filepath.Abs(ExpandPath(h, home))
		if err != nil {
			return Paths{}, err
		}
		p.ConfigDir = abs
		p.HumHome = abs
	}
	return p, nil
}

func (p Paths) ServicesDir() string { return filepath.Join(p.ConfigDir, "services") }

func (p Paths) ServiceFile(name string) string {
	return filepath.Join(p.ServicesDir(), name+".toml")
}

func (p Paths) GlobalFile() string { return filepath.Join(p.ConfigDir, "config.toml") }

func (p Paths) PlistFile(name string) string {
	return filepath.Join(p.LaunchAgents, Label(name)+".plist")
}

func (p Paths) LogFile(name string) string { return filepath.Join(p.LogDir, name+".log") }

// ServiceNames lists services that have a config file, sorted.
func (p Paths) ServiceNames() ([]string, error) {
	entries, err := os.ReadDir(p.ServicesDir())
	if os.IsNotExist(err) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	var names []string
	for _, e := range entries {
		n := e.Name()
		if e.IsDir() || !strings.HasSuffix(n, ".toml") {
			continue
		}
		names = append(names, strings.TrimSuffix(n, ".toml"))
	}
	sort.Strings(names)
	return names, nil
}

// PlistNames lists services that have a hum plist in LaunchAgents, sorted.
// The internal rotate job is excluded.
func (p Paths) PlistNames() ([]string, error) {
	entries, err := os.ReadDir(p.LaunchAgents)
	if os.IsNotExist(err) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	var names []string
	for _, e := range entries {
		n := e.Name()
		if !strings.HasPrefix(n, LabelPrefix) || !strings.HasSuffix(n, ".plist") {
			continue
		}
		name := strings.TrimSuffix(strings.TrimPrefix(n, LabelPrefix), ".plist")
		if name == RotateName {
			continue
		}
		names = append(names, name)
	}
	sort.Strings(names)
	return names, nil
}

// ExpandPath expands a leading "~" and "$HOME" / "${HOME}".
func ExpandPath(path, home string) string {
	if path == "~" {
		return home
	}
	if strings.HasPrefix(path, "~/") {
		path = filepath.Join(home, path[2:])
	}
	path = strings.ReplaceAll(path, "${HOME}", home)
	path = strings.ReplaceAll(path, "$HOME", home)
	return path
}

// ContractPath rewrites an absolute path under home to the "~/..." form.
func ContractPath(path, home string) string {
	if path == home {
		return "~"
	}
	if rel, err := filepath.Rel(home, path); err == nil && !strings.HasPrefix(rel, "..") {
		return "~/" + filepath.ToSlash(rel)
	}
	return path
}

// WriteFileAtomic writes data to a temp file next to path and renames it.
func WriteFileAtomic(path string, data []byte, perm os.FileMode) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}
	if info, err := os.Stat(path); err == nil {
		perm = info.Mode().Perm()
	}
	f, err := os.CreateTemp(filepath.Dir(path), "."+filepath.Base(path)+".tmp*")
	if err != nil {
		return err
	}
	tmp := f.Name()
	defer os.Remove(tmp)
	if _, err := f.Write(data); err != nil {
		f.Close()
		return err
	}
	if err := f.Chmod(perm); err != nil {
		f.Close()
		return err
	}
	if err := f.Close(); err != nil {
		return err
	}
	return os.Rename(tmp, path)
}
