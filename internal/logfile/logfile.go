// Package logfile rotates and reads service logs.
//
// Rotation is copytruncate: launchd opens the log once when it starts the
// job and hands the descriptor to the process, so renaming the file would
// leave the process writing to the renamed copy. Lines written between
// the copy and the truncate are lost.
package logfile

import (
	"bufio"
	"bytes"
	"context"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"
)

// Rotate shifts <path>.1 … <path>.<keep-1> up by one, drops anything past
// keep, copies path to <path>.1 and truncates path. A missing log is not an
// error.
func Rotate(path string, keep int) error {
	if _, err := os.Stat(path); os.IsNotExist(err) {
		return nil
	} else if err != nil {
		return err
	}
	if err := pruneAbove(path, keep); err != nil {
		return err
	}
	if keep > 0 {
		for i := keep - 1; i >= 1; i-- {
			from := path + "." + strconv.Itoa(i)
			if _, err := os.Stat(from); err == nil {
				if err := os.Rename(from, path+"."+strconv.Itoa(i+1)); err != nil {
					return err
				}
			}
		}
		if err := copyFile(path, path+".1"); err != nil {
			return err
		}
	}
	return os.Truncate(path, 0)
}

// RotateIfNeeded rotates when the log is larger than maxSize. It reports
// whether it rotated.
func RotateIfNeeded(path string, maxSize int64, keep int) (bool, error) {
	info, err := os.Stat(path)
	if os.IsNotExist(err) {
		return false, nil
	}
	if err != nil {
		return false, err
	}
	if info.Size() <= maxSize {
		return false, nil
	}
	return true, Rotate(path, keep)
}

// pruneAbove removes <path>.N for N >= keep, so that after the shift at
// most keep history files remain.
func pruneAbove(path string, keep int) error {
	matches, err := filepath.Glob(path + ".*")
	if err != nil {
		return err
	}
	for _, m := range matches {
		n, err := strconv.Atoi(strings.TrimPrefix(m, path+"."))
		if err != nil || n < keep || n < 1 {
			continue
		}
		if err := os.Remove(m); err != nil && !os.IsNotExist(err) {
			return err
		}
	}
	return nil
}

func copyFile(src, dst string) error {
	in, err := os.Open(src)
	if err != nil {
		return err
	}
	defer in.Close()
	tmp := dst + ".tmp"
	out, err := os.OpenFile(tmp, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, 0o644)
	if err != nil {
		return err
	}
	if _, err := io.Copy(out, in); err != nil {
		out.Close()
		os.Remove(tmp)
		return err
	}
	if err := out.Close(); err != nil {
		os.Remove(tmp)
		return err
	}
	return os.Rename(tmp, dst)
}

// Tail returns the last n lines of the file and the offset it read up to.
// A missing file yields no lines.
func Tail(path string, n int) ([]string, int64, error) {
	f, err := os.Open(path)
	if os.IsNotExist(err) {
		return nil, 0, nil
	}
	if err != nil {
		return nil, 0, err
	}
	defer f.Close()
	size, err := f.Seek(0, io.SeekEnd)
	if err != nil {
		return nil, 0, err
	}
	if n <= 0 || size == 0 {
		return nil, size, nil
	}
	const chunk = 64 << 10
	var buf []byte
	pos := size
	for pos > 0 && bytes.Count(buf, []byte("\n")) <= n {
		step := int64(chunk)
		if pos < step {
			step = pos
		}
		pos -= step
		b := make([]byte, step)
		if _, err := f.ReadAt(b, pos); err != nil && err != io.EOF {
			return nil, 0, err
		}
		buf = append(b, buf...)
	}
	text := strings.TrimSuffix(string(buf), "\n")
	lines := strings.Split(text, "\n")
	if pos > 0 && len(lines) > 0 {
		lines = lines[1:] // first line may be partial
	}
	if len(lines) > n {
		lines = lines[len(lines)-n:]
	}
	return lines, size, nil
}

// Follow streams complete lines appended after offset until ctx ends. A
// file that shrinks (rotated by truncation) is read again from the start.
func Follow(ctx context.Context, path string, offset int64, emit func(line string)) error {
	var partial []byte
	ticker := time.NewTicker(250 * time.Millisecond)
	defer ticker.Stop()
	for {
		info, err := os.Stat(path)
		if err == nil {
			if info.Size() < offset {
				offset = 0
				partial = nil
			}
			if info.Size() > offset {
				f, err := os.Open(path)
				if err != nil {
					return err
				}
				if _, err := f.Seek(offset, io.SeekStart); err != nil {
					f.Close()
					return err
				}
				r := bufio.NewReader(f)
				for {
					b, err := r.ReadBytes('\n')
					offset += int64(len(b))
					if err != nil {
						partial = append(partial, b...)
						break
					}
					line := append(partial, b[:len(b)-1]...)
					partial = nil
					emit(string(line))
				}
				f.Close()
			}
		} else if !os.IsNotExist(err) {
			return fmt.Errorf("follow %s: %w", path, err)
		}
		select {
		case <-ctx.Done():
			return nil
		case <-ticker.C:
		}
	}
}
