package envx

import (
	"errors"
	"os"
	"path/filepath"
	"testing"

	"github.com/plimeor/hum/internal/config"
)

func TestAssemblePriorityAndSources(t *testing.T) {
	home := t.TempDir()
	cwd := filepath.Join(home, "work")
	os.MkdirAll(cwd, 0o755)
	os.WriteFile(filepath.Join(cwd, ".env"), []byte("A=env1\nB=env1\nDATA=${HOME}/data\nP=$PATH:/extra\n"), 0o644)
	os.WriteFile(filepath.Join(cwd, ".env.local"), []byte("B=env2\nC=$A-$SVC\n"), 0o644)

	base := map[string]string{"HOME": home, "USER": "me"}
	g := &config.Global{Env: map[string]string{"PATH": "/usr/bin", "A": "global", "LANG": "en_US.UTF-8"}}
	s := &config.Service{
		Cwd:     cwd,
		EnvFile: []string{".env", ".env.local"},
		Env:     map[string]string{"A": "svc", "SVC": "literal $HOME"},
	}
	e, err := Assemble(base, g, s, home)
	if err != nil {
		t.Fatal(err)
	}
	want := map[string][2]string{
		"USER": {"me", SourceBase},
		"LANG": {"en_US.UTF-8", SourceGlobal},
		"PATH": {"/usr/bin", SourceGlobal},
		"A":    {"svc", SourceService},
		"B":    {"env2", "~/work/.env.local"},
		"DATA": {home + "/data", "~/work/.env"},
		"P":    {"/usr/bin:/extra", "~/work/.env"},
		// $A resolves to the .env value, $SVC is the service's own [env]
		// and is not visible to env files.
		"C":   {"env1-", "~/work/.env.local"},
		"SVC": {"literal $HOME", SourceService},
	}
	for k, w := range want {
		if e.Vars[k] != w[0] || e.Sources[k] != w[1] {
			t.Errorf("%s = %q from %q, want %q from %q", k, e.Vars[k], e.Sources[k], w[0], w[1])
		}
	}
	if len(e.Undefined) != 1 || e.Undefined[0].Name != "SVC" {
		t.Errorf("undefined = %+v", e.Undefined)
	}
	if _, ok := e.Vars["SHELL"]; ok {
		t.Error("caller variables must not leak in")
	}
}

func TestAssembleMissingEnvFile(t *testing.T) {
	home := t.TempDir()
	s := &config.Service{Cwd: home, EnvFile: []string{".env"}}
	_, err := Assemble(nil, nil, s, home)
	var missing *MissingEnvFileError
	if !errors.As(err, &missing) || missing.Path != filepath.Join(home, ".env") {
		t.Fatalf("err = %v", err)
	}
}

func TestLookPath(t *testing.T) {
	dir := t.TempDir()
	bin := filepath.Join(dir, "tool")
	os.WriteFile(bin, []byte("#!/bin/sh\n"), 0o755)
	os.WriteFile(filepath.Join(dir, "noexec"), []byte(""), 0o644)
	if got, err := LookPath("tool", "/nonexistent:"+dir); err != nil || got != bin {
		t.Fatalf("got %q %v", got, err)
	}
	if _, err := LookPath("noexec", dir); err == nil {
		t.Fatal("non-executable must not match")
	}
	if _, err := LookPath("tool", ""); err == nil {
		t.Fatal("empty PATH must not find it")
	}
	if got, err := LookPath(bin, ""); err != nil || got != bin {
		t.Fatal("absolute path should be used directly")
	}
}
