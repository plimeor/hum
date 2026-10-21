// Package envx assembles a service's environment from scratch.
//
// Layers, lowest priority first: base variables, global [env], the
// service's env_file entries in order, the service's [env].
package envx

import (
	"fmt"
	"os"
	"os/user"
	"path/filepath"
	"sort"
	"strings"

	"github.com/plimeor/hum/internal/config"
	"github.com/plimeor/hum/internal/dotenv"
)

// BaseKeys are the only variables taken from the calling process.
var BaseKeys = []string{"HOME", "USER", "SHELL", "TMPDIR", "LANG"}

// Source labels for layers that are not files.
const (
	SourceBase    = "base"
	SourceGlobal  = "global [env]"
	SourceService = "service [env]"
)

// MissingEnvFileError reports an env_file that cannot be read.
type MissingEnvFileError struct {
	Path string
	Err  error
}

func (e *MissingEnvFileError) Error() string {
	return fmt.Sprintf("env_file %s: %v", e.Path, e.Err)
}

func (e *MissingEnvFileError) Unwrap() error { return e.Err }

// Undefined is an unresolved reference inside an env file.
type Undefined struct {
	File string
	Line int
	Name string
}

// Env is an assembled environment.
type Env struct {
	Vars      map[string]string
	Sources   map[string]string // key -> layer that set the final value
	Undefined []Undefined
}

// Keys returns variable names, sorted.
func (e *Env) Keys() []string {
	keys := make([]string, 0, len(e.Vars))
	for k := range e.Vars {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	return keys
}

// Environ returns the environment in KEY=VALUE form, sorted.
func (e *Env) Environ() []string {
	keys := e.Keys()
	out := make([]string, len(keys))
	for i, k := range keys {
		out[i] = k + "=" + e.Vars[k]
	}
	return out
}

// Base reads the base variables from the current process. HOME and USER
// fall back to the user database when unset.
func Base() map[string]string {
	base := map[string]string{}
	for _, k := range BaseKeys {
		if v, ok := os.LookupEnv(k); ok {
			base[k] = v
		}
	}
	if base["HOME"] == "" || base["USER"] == "" {
		if u, err := user.Current(); err == nil {
			if base["HOME"] == "" {
				base["HOME"] = u.HomeDir
			}
			if base["USER"] == "" {
				base["USER"] = u.Username
			}
		}
	}
	return base
}

// Assemble builds the environment for s. On a missing or unreadable
// env_file it returns a *MissingEnvFileError together with the layers
// assembled so far.
func Assemble(base map[string]string, g *config.Global, s *config.Service, home string) (*Env, error) {
	e := &Env{Vars: map[string]string{}, Sources: map[string]string{}}
	set := func(k, v, src string) {
		e.Vars[k] = v
		e.Sources[k] = src
	}
	for k, v := range base {
		set(k, v, SourceBase)
	}
	if g != nil {
		for k, v := range g.Env {
			set(k, v, SourceGlobal)
		}
	}
	for _, path := range s.ResolvedEnvFiles(home) {
		data, err := os.ReadFile(path)
		if err != nil {
			return e, &MissingEnvFileError{Path: path, Err: unwrapPathErr(err)}
		}
		res, err := dotenv.Parse(data, func(name string) (string, bool) {
			v, ok := e.Vars[name]
			return v, ok
		})
		if err != nil {
			return e, fmt.Errorf("env_file %s: %w", path, err)
		}
		label := config.ContractPath(path, home)
		for _, kv := range res.Vars {
			set(kv.Key, kv.Value, label)
		}
		for _, u := range res.Undefined {
			e.Undefined = append(e.Undefined, Undefined{File: label, Line: u.Line, Name: u.Name})
		}
	}
	for k, v := range s.Env {
		set(k, v, SourceService)
	}
	return e, nil
}

func unwrapPathErr(err error) error {
	if pe, ok := err.(*os.PathError); ok {
		return pe.Err
	}
	return err
}

// LookPath finds file in the PATH value path, like exec.LookPath but
// against the assembled environment instead of the caller's.
func LookPath(file, path string) (string, error) {
	if strings.Contains(file, "/") {
		if isExecutable(file) {
			return file, nil
		}
		return "", fmt.Errorf("%s: not an executable file", file)
	}
	for _, dir := range filepath.SplitList(path) {
		if dir == "" {
			dir = "."
		}
		p := filepath.Join(dir, file)
		if isExecutable(p) {
			return p, nil
		}
	}
	return "", fmt.Errorf("%s: not found in PATH", file)
}

func isExecutable(p string) bool {
	info, err := os.Stat(p)
	return err == nil && !info.IsDir() && info.Mode()&0o111 != 0
}
