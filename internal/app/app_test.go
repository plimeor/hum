package app

import (
	"bytes"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/plimeor/hum/internal/config"
	"github.com/plimeor/hum/internal/envx"
	"github.com/plimeor/hum/internal/launchd"
)

// fakeLD models the parts of launchd hum relies on.
type fakeLD struct {
	jobs     map[string]*launchd.Status
	disabled map[string]bool
	calls    []string
}

func newFake() *fakeLD {
	return &fakeLD{jobs: map[string]*launchd.Status{}, disabled: map[string]bool{}}
}

func (f *fakeLD) log(format string, args ...any) {
	f.calls = append(f.calls, fmt.Sprintf(format, args...))
}

func (f *fakeLD) Status(label string) (launchd.Status, error) {
	if st, ok := f.jobs[label]; ok {
		return *st, nil
	}
	return launchd.Status{}, nil
}

func (f *fakeLD) Disabled() (map[string]bool, error) { return f.disabled, nil }

func (f *fakeLD) Bootstrap(path string) error {
	label := strings.TrimSuffix(filepath.Base(path), ".plist")
	f.log("bootstrap %s", label)
	if f.disabled[label] {
		return errors.New("service is disabled")
	}
	if f.jobs[label] != nil {
		return errors.New("already loaded")
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return err
	}
	runAtLoad := bytes.Contains(data, []byte("<key>RunAtLoad</key>\n\t<true/>"))
	st := &launchd.Status{Loaded: true, Path: path}
	if runAtLoad {
		st.Running, st.PID = true, 100
	}
	f.jobs[label] = st
	return nil
}

func (f *fakeLD) Bootout(label string) error {
	if f.jobs[label] != nil {
		f.log("bootout %s", label)
	}
	delete(f.jobs, label)
	return nil
}

func (f *fakeLD) Enable(label string) error { delete(f.disabled, label); return nil }
func (f *fakeLD) Disable(label string) error {
	f.disabled[label] = true
	f.log("disable %s", label)
	return nil
}

func (f *fakeLD) Kickstart(label string) error {
	st := f.jobs[label]
	if st == nil {
		return errors.New("not loaded")
	}
	if !st.Running {
		f.log("kickstart %s", label)
		st.Running, st.PID = true, 200
	}
	return nil
}

func newTestApp(t *testing.T) (*App, *fakeLD) {
	t.Helper()
	home := t.TempDir()
	p := config.Paths{
		Home:         home,
		ConfigDir:    filepath.Join(home, ".config", "hum"),
		LaunchAgents: filepath.Join(home, "Library", "LaunchAgents"),
		LogDir:       filepath.Join(home, "Library", "Logs", "hum"),
	}
	if _, err := config.EnsureGlobal(p, "/usr/bin:/bin"); err != nil {
		t.Fatal(err)
	}
	ld := newFake()
	return &App{Paths: p, LD: ld, HumPath: "/opt/homebrew/bin/hum", Out: io.Discard, Err: io.Discard}, ld
}

func writeService(t *testing.T, a *App, name, body string) {
	t.Helper()
	if err := config.WriteFileAtomic(a.Paths.ServiceFile(name), []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
}

func TestStartStopRestartRemove(t *testing.T) {
	a, ld := newTestApp(t)
	writeService(t, a, "t3", `command = ["sleep", "1000"]`)

	if err := a.Start("t3"); err != nil {
		t.Fatal(err)
	}
	st, _ := ld.Status("hum.t3")
	if !st.Running {
		t.Fatal("not running after start")
	}
	if _, err := os.Stat(a.Paths.PlistFile("t3")); err != nil {
		t.Fatal("plist not written")
	}
	if _, err := os.Stat(a.Paths.PlistFile(config.RotateName)); err != nil {
		t.Fatal("rotate job not installed")
	}
	if !ld.jobs["hum._rotate"].Loaded {
		t.Fatal("rotate job not loaded")
	}

	// Start on a running service is a no-op.
	ld.calls = nil
	if err := a.Start("t3"); err != nil || len(ld.calls) != 0 {
		t.Fatalf("second start: %v %v", err, ld.calls)
	}

	if err := a.Stop("t3"); err != nil {
		t.Fatal(err)
	}
	rows, _ := a.States()
	if rows[0].State != StateDisabled {
		t.Fatalf("state after stop = %s", rows[0].State)
	}

	// Restart of a stopped service re-enables and runs it.
	if err := a.Restart("t3"); err != nil {
		t.Fatal(err)
	}
	if st, _ := ld.Status("hum.t3"); !st.Running || ld.disabled["hum.t3"] {
		t.Fatalf("restart: %+v disabled=%v", st, ld.disabled["hum.t3"])
	}

	if err := a.Remove("t3", false); err != nil {
		t.Fatal(err)
	}
	for _, p := range []string{a.Paths.PlistFile("t3"), a.Paths.ServiceFile("t3"), a.Paths.PlistFile(config.RotateName)} {
		if _, err := os.Stat(p); !os.IsNotExist(err) {
			t.Errorf("%s still exists", p)
		}
	}
	if ld.jobs["hum.t3"] != nil || ld.jobs["hum._rotate"] != nil {
		t.Error("jobs still loaded after rm")
	}
}

func TestStartWithoutAutostartKickstarts(t *testing.T) {
	a, ld := newTestApp(t)
	writeService(t, a, "x", "command = [\"sleep\", \"1\"]\nautostart = false\n")
	if err := a.Start("x"); err != nil {
		t.Fatal(err)
	}
	if st, _ := ld.Status("hum.x"); !st.Running {
		t.Fatal("autostart=false service must still run on explicit start")
	}
}

func TestStartReloadsOutdatedLoadedJob(t *testing.T) {
	a, ld := newTestApp(t)
	writeService(t, a, "x", "command = [\"sleep\", \"1\"]\nautostart = false\n")
	if err := a.Start("x"); err != nil {
		t.Fatal(err)
	}
	ld.jobs["hum.x"].Running = false // exited
	writeService(t, a, "x", "command = [\"sleep\", \"1\"]\nautostart = false\nrestart = \"never\"\n")
	ld.calls = nil
	if err := a.Start("x"); err != nil {
		t.Fatal(err)
	}
	if strings.Join(ld.calls, ";") != "bootout hum.x;bootstrap hum.x;kickstart hum.x" {
		t.Fatalf("calls = %v", ld.calls)
	}
}

func TestStatesAndApply(t *testing.T) {
	a, ld := newTestApp(t)
	writeService(t, a, "new1", `command = ["sleep", "1"]`)
	writeService(t, a, "changed", `command = ["sleep", "1"]`)
	writeService(t, a, "broken", `command = 3`)
	if err := a.Start("changed"); err != nil {
		t.Fatal(err)
	}
	writeService(t, a, "changed", "command = [\"sleep\", \"2\"]\nrestart = \"on-failure\"\n")
	// A plist left behind by a deleted config.
	os.WriteFile(a.Paths.PlistFile("orphan"), []byte("<plist/>"), 0o644)
	ld.jobs["hum.orphan"] = &launchd.Status{Loaded: true, Running: true, PID: 9}

	rows, err := a.States()
	if err != nil {
		t.Fatal(err)
	}
	byName := map[string]ServiceState{}
	for _, r := range rows {
		byName[r.Name] = r
	}
	if !byName["new1"].OutOfSync || byName["new1"].Reason != "plist missing" {
		t.Errorf("new1: %+v", byName["new1"])
	}
	if !byName["changed"].OutOfSync || byName["changed"].State != StateRunning {
		t.Errorf("changed: %+v", byName["changed"])
	}
	if byName["broken"].Error == "" {
		t.Errorf("broken: %+v", byName["broken"])
	}
	if !byName["orphan"].OutOfSync {
		t.Errorf("orphan: %+v", byName["orphan"])
	}

	actions, problems, err := a.Plan()
	if err != nil {
		t.Fatal(err)
	}
	if len(problems) != 1 {
		t.Errorf("problems = %v", problems)
	}
	got := fmt.Sprint(actions)
	if got != "[{update changed} {register new1} {unregister orphan}]" {
		t.Fatalf("plan = %s", got)
	}
	if err := a.Apply(actions); err != nil {
		t.Fatal(err)
	}
	if st, _ := ld.Status("hum.changed"); !st.Running {
		t.Error("changed service should be running again after reload")
	}
	if st, _ := ld.Status("hum.new1"); !st.Running {
		t.Error("new1 should be registered and started (autostart)")
	}
	if ld.jobs["hum.orphan"] != nil {
		t.Error("orphan not unloaded")
	}
	if _, err := os.Stat(a.Paths.PlistFile("orphan")); !os.IsNotExist(err) {
		t.Error("orphan plist not removed")
	}
	actions, _, _ = a.Plan()
	if len(actions) != 0 {
		t.Errorf("second plan not empty: %v", actions)
	}
}

func TestApplyConfigChange(t *testing.T) {
	a, ld := newTestApp(t)
	writeService(t, a, "x", `command = ["sleep", "1"]`)
	if err := a.Start("x"); err != nil {
		t.Fatal(err)
	}
	writeService(t, a, "x", "command = [\"sleep\", \"1\"]\nautostart = false\n")
	ld.calls = nil
	if err := a.ApplyConfigChange("x", false); err != nil || len(ld.calls) != 0 {
		t.Fatalf("--no-restart must not touch launchd: %v %v", err, ld.calls)
	}
	if err := a.ApplyConfigChange("x", true); err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(strings.Join(ld.calls, ";"), "bootout hum.x;bootstrap hum.x") {
		t.Fatalf("calls = %v", ld.calls)
	}
	if _, current := a.PlistCurrent(mustLoad(t, a, "x")); !current {
		t.Fatal("plist not regenerated")
	}
}

func mustLoad(t *testing.T, a *App, name string) *config.Service {
	t.Helper()
	s, err := config.LoadService(a.Paths, name)
	if err != nil {
		t.Fatal(err)
	}
	return s
}

func TestPrepareRun(t *testing.T) {
	a, _ := newTestApp(t)
	cwd := filepath.Join(a.Paths.Home, "work")
	os.MkdirAll(cwd, 0o755)
	os.WriteFile(filepath.Join(cwd, ".env"), []byte("FROM_FILE=${HOME}/x\n"), 0o644)
	writeService(t, a, "t3", "command = [\"sh\", \"-c\", \"env\"]\ncwd = \"~/work\"\nenv_file = [\".env\"]\n[env]\nPORT = \"3773\"\n")

	ex, err := a.PrepareRun("t3", map[string]string{"HOME": a.Paths.Home, "USER": "me"})
	if err != nil {
		t.Fatal(err)
	}
	if ex.Path != "/usr/bin/sh" && ex.Path != "/bin/sh" {
		t.Errorf("path = %s", ex.Path)
	}
	if ex.Dir != cwd {
		t.Errorf("dir = %s", ex.Dir)
	}
	env := strings.Join(ex.Env, "\n")
	for _, want := range []string{"PATH=/usr/bin:/bin", "PORT=3773", "FROM_FILE=" + a.Paths.Home + "/x", "USER=me"} {
		if !strings.Contains(env, want) {
			t.Errorf("env missing %s:\n%s", want, env)
		}
	}

	os.Remove(filepath.Join(cwd, ".env"))
	_, err = a.PrepareRun("t3", nil)
	var missing *envx.MissingEnvFileError
	if !errors.As(err, &missing) {
		t.Fatalf("missing env_file: err = %v", err)
	}
}

func TestPrepareRunRotatesOversizedLog(t *testing.T) {
	a, _ := newTestApp(t)
	writeService(t, a, "x", "command = [\"sh\"]\n[log]\nmax_size = \"10\"\nkeep = 2\n")
	os.MkdirAll(a.Paths.LogDir, 0o755)
	os.WriteFile(a.Paths.LogFile("x"), []byte("more than ten bytes"), 0o644)
	if _, err := a.PrepareRun("x", nil); err != nil {
		t.Fatal(err)
	}
	if b, _ := os.ReadFile(a.Paths.LogFile("x") + ".1"); string(b) != "more than ten bytes" {
		t.Fatal("log not rotated before start")
	}
}

func TestDoctor(t *testing.T) {
	a, _ := newTestApp(t)
	writeService(t, a, "bad", "command = [\"definitely-not-a-command\"]\ncwd = \"/nonexistent\"\nenv_file = [\".env\"]\n")
	fs, err := a.Doctor()
	if err != nil {
		t.Fatal(err)
	}
	var msgs []string
	for _, f := range fs {
		if f.Subject == "bad" {
			msgs = append(msgs, f.Severity+": "+f.Message)
		}
	}
	joined := strings.Join(msgs, "\n")
	for _, want := range []string{"not found in PATH", "cwd /nonexistent does not exist", "env_file /nonexistent/.env", "plist missing"} {
		if !strings.Contains(joined, want) {
			t.Errorf("doctor missing %q:\n%s", want, joined)
		}
	}
}
