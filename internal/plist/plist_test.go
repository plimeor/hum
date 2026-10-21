package plist

import (
	"flag"
	"os"
	"path/filepath"
	"testing"
)

var update = flag.Bool("update", false, "rewrite golden files")

func golden(t *testing.T, name string, got []byte) {
	t.Helper()
	path := filepath.Join("testdata", name)
	if *update {
		if err := os.WriteFile(path, got, 0o644); err != nil {
			t.Fatal(err)
		}
	}
	want, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if string(got) != string(want) {
		t.Fatalf("%s mismatch:\n%s", name, got)
	}
}

func TestRenderAlways(t *testing.T) {
	golden(t, "always.plist", Render(Job{
		Label:            "hum.t3",
		ProgramArguments: []string{"/opt/homebrew/bin/hum", "run", "t3"},
		RunAtLoad:        true,
		KeepAlive:        KeepAliveAlways,
		LogPath:          "/Users/me/Library/Logs/hum/t3.log",
	}))
}

func TestRenderOnFailureNoAutostart(t *testing.T) {
	golden(t, "on-failure.plist", Render(Job{
		Label:            "hum.api",
		ProgramArguments: []string{"/opt/homebrew/bin/hum", "run", "api"},
		KeepAlive:        KeepAliveOnFailure,
		LogPath:          "/Users/me/Library/Logs/hum/api.log",
		Env:              map[string]string{"HUM_HOME": "/Users/me/dot & files/hum"},
	}))
}

func TestRenderNeverWithInterval(t *testing.T) {
	golden(t, "rotate.plist", Render(Job{
		Label:            "hum._rotate",
		ProgramArguments: []string{"/opt/homebrew/bin/hum", "logs", "--rotate", "--all", "--if-oversize"},
		StartInterval:    3600,
		LogPath:          "/Users/me/Library/Logs/hum/_rotate.log",
	}))
}

func TestProgram(t *testing.T) {
	data := Render(Job{Label: "x", ProgramArguments: []string{"/a & b/hum", "run", "x"}})
	got, ok := Program(data)
	if !ok || got != "/a & b/hum" {
		t.Fatalf("Program = %q %v", got, ok)
	}
}
