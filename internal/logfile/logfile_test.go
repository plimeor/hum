package logfile

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"
)

func read(t *testing.T, p string) string {
	t.Helper()
	b, err := os.ReadFile(p)
	if err != nil {
		t.Fatal(err)
	}
	return string(b)
}

var t0 = time.Date(2026, 10, 21, 14, 0, 0, 0, time.Local)

func TestRotateNamingRetainAndTruncate(t *testing.T) {
	dir := t.TempDir()
	p := filepath.Join(dir, "t3.log")
	// A service named "t3__x" must not be mistaken for a rotated t3 log.
	os.WriteFile(filepath.Join(dir, "t3__x.log"), []byte("other service"), 0o644)
	for i, content := range []string{"one", "two", "three", "four"} {
		os.WriteFile(p, []byte(content), 0o644)
		// Keep the inode: rotation must truncate, not replace, the file.
		before, _ := os.Stat(p)
		rotated, err := Rotate(p, 3, t0.Add(time.Duration(i)*time.Hour))
		if err != nil || !rotated {
			t.Fatal(rotated, err)
		}
		after, _ := os.Stat(p)
		if !os.SameFile(before, after) || after.Size() != 0 {
			t.Fatalf("round %d: log must be truncated in place", i)
		}
	}
	files, err := Rotated(p)
	if err != nil {
		t.Fatal(err)
	}
	var names []string
	for _, f := range files {
		names = append(names, filepath.Base(f))
	}
	want := "t3__2026-10-21_17-00-00.log,t3__2026-10-21_16-00-00.log,t3__2026-10-21_15-00-00.log"
	if strings.Join(names, ",") != want {
		t.Fatalf("rotated = %v", names)
	}
	if read(t, files[0]) != "four" || read(t, files[2]) != "two" {
		t.Fatal("contents in wrong files")
	}
	if read(t, filepath.Join(dir, "t3__x.log")) != "other service" {
		t.Fatal("another service's log was touched")
	}
}

func TestRotateSameSecond(t *testing.T) {
	p := filepath.Join(t.TempDir(), "x.log")
	for _, c := range []string{"a", "b", "c"} {
		os.WriteFile(p, []byte(c), 0o644)
		if _, err := Rotate(p, 5, t0); err != nil {
			t.Fatal(err)
		}
	}
	files, _ := Rotated(p)
	if len(files) != 3 || read(t, files[0]) != "c" || read(t, files[2]) != "a" {
		t.Fatalf("files = %v", files)
	}
	if !strings.HasSuffix(files[0], "x__2026-10-21_14-00-00_2.log") {
		t.Fatalf("newest = %s", files[0])
	}
}

func TestRotateEmptyAndKeepZero(t *testing.T) {
	p := filepath.Join(t.TempDir(), "x.log")
	os.WriteFile(p, nil, 0o644)
	if rotated, err := Rotate(p, 3, t0); rotated || err != nil {
		t.Fatal("empty log must not rotate")
	}
	os.WriteFile(p, []byte("data"), 0o644)
	if _, err := Rotate(p, 0, t0); err != nil {
		t.Fatal(err)
	}
	if read(t, p) != "" {
		t.Fatal("not truncated")
	}
	if files, _ := Rotated(p); len(files) != 0 {
		t.Fatalf("keep=0 left %v", files)
	}
}

func TestRotateIfNeeded(t *testing.T) {
	p := filepath.Join(t.TempDir(), "x.log")
	if rotated, err := RotateIfNeeded(p, 10, 3, t0); rotated || err != nil {
		t.Fatal("missing file must be a no-op")
	}
	os.WriteFile(p, []byte("small"), 0o644)
	if rotated, _ := RotateIfNeeded(p, 10, 3, t0); rotated {
		t.Fatal("rotated below max size")
	}
	os.WriteFile(p, []byte("exactly10!"), 0o644)
	if rotated, _ := RotateIfNeeded(p, 10, 3, t0); !rotated {
		t.Fatal("did not rotate at max size")
	}
}

func TestTail(t *testing.T) {
	p := filepath.Join(t.TempDir(), "x.log")
	var b strings.Builder
	for i := 0; i < 20000; i++ {
		b.WriteString("line ")
		b.WriteString(strings.Repeat("x", i%7))
		b.WriteString("\n")
	}
	b.WriteString("last\n")
	os.WriteFile(p, []byte(b.String()), 0o644)
	lines, off, err := Tail(p, 3)
	if err != nil {
		t.Fatal(err)
	}
	if len(lines) != 3 || lines[2] != "last" || off != int64(b.Len()) {
		t.Fatalf("lines = %q off=%d", lines, off)
	}
	if lines, _, _ := Tail(filepath.Join(t.TempDir(), "none"), 5); lines != nil {
		t.Fatal("missing file should be empty")
	}
}

func TestFollowSurvivesTruncate(t *testing.T) {
	p := filepath.Join(t.TempDir(), "x.log")
	os.WriteFile(p, []byte("old\n"), 0o644)
	ctx, cancel := context.WithCancel(context.Background())
	var mu sync.Mutex
	var got []string
	done := make(chan struct{})
	go func() {
		Follow(ctx, p, 4, func(l string) { mu.Lock(); got = append(got, l); mu.Unlock() })
		close(done)
	}()
	f, _ := os.OpenFile(p, os.O_APPEND|os.O_WRONLY, 0)
	f.WriteString("a\npart")
	time.Sleep(400 * time.Millisecond)
	f.WriteString("ial\n")
	time.Sleep(400 * time.Millisecond)
	os.Truncate(p, 0)
	time.Sleep(400 * time.Millisecond)
	f.WriteString("after\n")
	f.Close()
	time.Sleep(400 * time.Millisecond)
	cancel()
	<-done
	mu.Lock()
	defer mu.Unlock()
	if strings.Join(got, ",") != "a,partial,after" {
		t.Fatalf("got %q", got)
	}
}
