// Package logfile rotates and reads service logs.
//
// Rotation follows pm2-logrotate: the log is copied to a timestamped file
// (<name>__YYYY-MM-DD_HH-mm-ss.log) and then truncated in place, and only
// the newest keep copies are retained. The log is never renamed, because
// launchd opens it once when it starts the job and hands the descriptor to
// the process. Lines written between the copy and the truncate are lost.
package logfile

import (
	"bufio"
	"bytes"
	"context"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strconv"
	"strings"
	"time"
)

// stampLayout is pm2-logrotate's default dateFormat, YYYY-MM-DD_HH-mm-ss.
const stampLayout = "2006-01-02_15-04-05"

var stampRe = regexp.MustCompile(`^__\d{4}-\d{2}-\d{2}_\d{2}-\d{2}-\d{2}(_\d+)?\.log$`)

// Rotated lists the rotated copies of the log at path, newest first.
func Rotated(path string) ([]string, error) {
	base := strings.TrimSuffix(filepath.Base(path), ".log")
	entries, err := os.ReadDir(filepath.Dir(path))
	if os.IsNotExist(err) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	var out []string
	for _, e := range entries {
		n := e.Name()
		// Match the exact stamp so that service "t3" never claims the
		// current log of a service named "t3__x".
		if strings.HasPrefix(n, base) && stampRe.MatchString(n[len(base):]) {
			out = append(out, filepath.Join(filepath.Dir(path), n))
		}
	}
	sort.Sort(sort.Reverse(byStamp(out)))
	return out, nil
}

// byStamp orders rotated names by timestamp, then by collision suffix.
type byStamp []string

func (b byStamp) Len() int      { return len(b) }
func (b byStamp) Swap(i, j int) { b[i], b[j] = b[j], b[i] }
func (b byStamp) Less(i, j int) bool {
	si, ni := splitStamp(b[i])
	sj, nj := splitStamp(b[j])
	if si != sj {
		return si < sj
	}
	return ni < nj
}

func splitStamp(name string) (string, int) {
	name = strings.TrimSuffix(name, ".log")
	stamp := name[strings.LastIndex(name, "__")+2:]
	if i := strings.IndexByte(stamp[len(stampLayout):], '_'); i >= 0 {
		n, _ := strconv.Atoi(stamp[len(stampLayout)+i+1:])
		return stamp[:len(stampLayout)], n
	}
	return stamp, 0
}

// Rotate copies a non-empty log to <name>__<now>.log, truncates it and
// deletes all but the newest keep copies. A missing or empty log is left
// alone. It reports whether it rotated.
func Rotate(path string, keep int, now time.Time) (bool, error) {
	info, err := os.Stat(path)
	if os.IsNotExist(err) {
		return false, nil
	}
	if err != nil {
		return false, err
	}
	if info.Size() == 0 {
		return false, nil
	}
	if keep > 0 {
		dst := rotatedName(path, now)
		if err := copyFile(path, dst); err != nil {
			return false, err
		}
	}
	if err := os.Truncate(path, 0); err != nil {
		return false, err
	}
	return true, prune(path, keep)
}

// RotateIfNeeded rotates when the log has reached maxSize.
func RotateIfNeeded(path string, maxSize int64, keep int, now time.Time) (bool, error) {
	info, err := os.Stat(path)
	if os.IsNotExist(err) {
		return false, nil
	}
	if err != nil {
		return false, err
	}
	if info.Size() < maxSize {
		return false, nil
	}
	return Rotate(path, keep, now)
}

// rotatedName picks a stamped name that is not taken yet; two rotations
// within one second get a _1, _2… suffix.
func rotatedName(path string, now time.Time) string {
	prefix := strings.TrimSuffix(path, ".log") + "__" + now.Format(stampLayout)
	name := prefix + ".log"
	for i := 1; ; i++ {
		if _, err := os.Stat(name); os.IsNotExist(err) {
			return name
		}
		name = prefix + "_" + strconv.Itoa(i) + ".log"
	}
}

func prune(path string, keep int) error {
	files, err := Rotated(path)
	if err != nil {
		return err
	}
	if keep < 0 {
		keep = 0
	}
	for i := keep; i < len(files); i++ {
		if err := os.Remove(files[i]); err != nil && !os.IsNotExist(err) {
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
