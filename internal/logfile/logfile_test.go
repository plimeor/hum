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

func TestRotateNumberingAndTruncate(t *testing.T) {
	p := filepath.Join(t.TempDir(), "t3.log")
	for i, content := range []string{"one", "two", "three", "four"} {
		os.WriteFile(p, []byte(content), 0o644)
		// Keep the inode: rotation must truncate, not replace, the file.
		before, _ := os.Stat(p)
		if err := Rotate(p, 3); err != nil {
			t.Fatal(err)
		}
		after, _ := os.Stat(p)
		if !os.SameFile(before, after) || after.Size() != 0 {
			t.Fatalf("round %d: log must be truncated in place", i)
		}
	}
	if read(t, p+".1") != "four" || read(t, p+".2") != "three" || read(t, p+".3") != "two" {
		t.Fatal("history order wrong")
	}
	if _, err := os.Stat(p + ".4"); !os.IsNotExist(err) {
		t.Fatal(".4 must not exist with keep=3")
	}
}

func TestRotateKeepLoweredPrunes(t *testing.T) {
	p := filepath.Join(t.TempDir(), "x.log")
	for _, s := range []string{"1", "2", "3", "4", "cur"} {
		suffix := "." + s
		if s == "cur" {
			suffix = ""
		}
		os.WriteFile(p+suffix, []byte(s), 0o644)
	}
	if err := Rotate(p, 1); err != nil {
		t.Fatal(err)
	}
	matches, _ := filepath.Glob(p + ".*")
	if len(matches) != 1 || read(t, p+".1") != "cur" {
		t.Fatalf("history = %v", matches)
	}
}

func TestRotateKeepZero(t *testing.T) {
	p := filepath.Join(t.TempDir(), "x.log")
	os.WriteFile(p, []byte("data"), 0o644)
	if err := Rotate(p, 0); err != nil {
		t.Fatal(err)
	}
	if read(t, p) != "" {
		t.Fatal("not truncated")
	}
	if m, _ := filepath.Glob(p + ".*"); len(m) != 0 {
		t.Fatalf("keep=0 left %v", m)
	}
}

func TestRotateIfNeeded(t *testing.T) {
	p := filepath.Join(t.TempDir(), "x.log")
	if rotated, err := RotateIfNeeded(p, 10, 3); rotated || err != nil {
		t.Fatal("missing file must be a no-op")
	}
	os.WriteFile(p, []byte("small"), 0o644)
	if rotated, _ := RotateIfNeeded(p, 10, 3); rotated {
		t.Fatal("rotated below max size")
	}
	os.WriteFile(p, []byte("larger than ten"), 0o644)
	if rotated, _ := RotateIfNeeded(p, 10, 3); !rotated {
		t.Fatal("did not rotate above max size")
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
