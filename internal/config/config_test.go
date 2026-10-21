package config

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestParseServiceDefaults(t *testing.T) {
	s, err := ParseService("t3", []byte(`command = ["t3", "server"]`))
	if err != nil {
		t.Fatal(err)
	}
	if !s.Autostart || s.Restart != RestartAlways || s.Shell != "" {
		t.Fatalf("defaults wrong: %+v", s)
	}
	if got := s.Argv(); strings.Join(got, " ") != "t3 server" {
		t.Fatalf("argv = %v", got)
	}
}

func TestParseServiceShellCommand(t *testing.T) {
	s, err := ParseService("x", []byte(`command = "echo hi && sleep 1"`))
	if err != nil {
		t.Fatal(err)
	}
	want := []string{"/bin/sh", "-c", "echo hi && sleep 1"}
	if got := s.Argv(); strings.Join(got, "|") != strings.Join(want, "|") {
		t.Fatalf("argv = %q", got)
	}
}

func TestParseServiceErrors(t *testing.T) {
	cases := map[string]string{
		"missing command": `cwd = "/tmp"`,
		"empty array":     `command = []`,
		"non-string arg":  `command = ["a", 1]`,
		"bad restart":     "command = [\"a\"]\nrestart = \"sometimes\"",
		"unknown field":   "command = [\"a\"]\nport = 1",
		"non-string env":  "command = [\"a\"]\n[env]\nPORT = 3773",
		"bad env key":     "command = [\"a\"]\n[env]\n\"A-B\" = \"1\"",
		"bad log size":    "command = [\"a\"]\n[log]\nmax_size = \"huge\"",
		"negative keep":   "command = [\"a\"]\n[log]\nkeep = -1",
	}
	for name, src := range cases {
		if _, err := ParseService("x", []byte(src)); err == nil {
			t.Errorf("%s: expected error", name)
		}
	}
}

func TestValidateName(t *testing.T) {
	for _, ok := range []string{"t3", "api-server", "a_b", "0x"} {
		if err := ValidateName(ok); err != nil {
			t.Errorf("%s: %v", ok, err)
		}
	}
	for _, bad := range []string{"", "_rotate", "-x", "A", "a.b", "a/b", "a b"} {
		if err := ValidateName(bad); err == nil {
			t.Errorf("%q: expected error", bad)
		}
	}
}

func TestParseSize(t *testing.T) {
	cases := map[string]int64{"10MB": 10 << 20, "512kb": 512 << 10, "1G": 1 << 30, "4096": 4096, "2MiB": 2 << 20}
	for in, want := range cases {
		got, err := ParseSize(in)
		if err != nil || got != want {
			t.Errorf("ParseSize(%q) = %d, %v; want %d", in, got, err, want)
		}
	}
	for _, bad := range []string{"", "10TB", "x", "0"} {
		if _, err := ParseSize(bad); err == nil {
			t.Errorf("ParseSize(%q): expected error", bad)
		}
	}
}

func TestEffectiveLog(t *testing.T) {
	size, keep := EffectiveLog(&Global{}, nil)
	if size != 10<<20 || keep != 3 {
		t.Fatalf("defaults: %d %d", size, keep)
	}
	five, one := 5, 1
	g := &Global{Log: LogSettings{MaxSize: "1MB", Keep: &five}}
	s := &Service{Log: LogSettings{Keep: &one}}
	size, keep = EffectiveLog(g, s)
	if size != 1<<20 || keep != 1 {
		t.Fatalf("override: %d %d", size, keep)
	}
}

func TestPathsExpandAndResolve(t *testing.T) {
	home := "/Users/me"
	if got := ExpandPath("~/work", home); got != "/Users/me/work" {
		t.Error(got)
	}
	if got := ExpandPath("$HOME/x", home); got != "/Users/me/x" {
		t.Error(got)
	}
	if got := ContractPath("/Users/me/work/t3", home); got != "~/work/t3" {
		t.Error(got)
	}
	if got := ContractPath("/opt/x", home); got != "/opt/x" {
		t.Error(got)
	}
	s := &Service{Cwd: "~/work/t3", EnvFile: []string{".env", "/etc/x.env", "~/shared.env"}}
	got := s.ResolvedEnvFiles(home)
	want := []string{"/Users/me/work/t3/.env", "/etc/x.env", "/Users/me/shared.env"}
	if strings.Join(got, ",") != strings.Join(want, ",") {
		t.Errorf("env files = %v", got)
	}
	if (&Service{}).ResolvedCwd(home) != home {
		t.Error("empty cwd should be home")
	}
}

func TestRenderServiceRoundTrip(t *testing.T) {
	s := &Service{
		Name: "api", Command: []string{"bun", "run", "start \"x\""}, Cwd: "~/work/api",
		EnvFile: []string{".env"}, Autostart: false, Restart: RestartOnFailure,
		Env: map[string]string{"PORT": "4000", "B": "a\\b"},
	}
	got, err := ParseService("api", RenderService(s))
	if err != nil {
		t.Fatal(err)
	}
	if strings.Join(got.Command, "|") != strings.Join(s.Command, "|") || got.Cwd != s.Cwd ||
		got.Autostart || got.Restart != RestartOnFailure || got.Env["B"] != "a\\b" || got.EnvFile[0] != ".env" {
		t.Fatalf("round trip mismatch: %+v", got)
	}
}

func TestEnsureGlobal(t *testing.T) {
	dir := t.TempDir()
	p := Paths{ConfigDir: dir}
	created, err := EnsureGlobal(p, "/opt/homebrew/bin:/usr/bin")
	if err != nil || !created {
		t.Fatal(created, err)
	}
	g, err := LoadGlobal(p)
	if err != nil {
		t.Fatal(err)
	}
	if g.Env["PATH"] != "/opt/homebrew/bin:/usr/bin" {
		t.Fatalf("PATH = %q", g.Env["PATH"])
	}
	if _, err := os.Stat(filepath.Join(dir, "services")); err != nil {
		t.Fatal("services dir not created")
	}
	created, err = EnsureGlobal(p, "/other")
	if err != nil || created {
		t.Fatal("second call must not recreate")
	}
}
