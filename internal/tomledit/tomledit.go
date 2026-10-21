// Package tomledit changes single keys in a TOML document in place, leaving
// every other byte (comments, ordering, formatting) untouched.
//
// It relies on hum's flat schema: top-level keys plus a few standard
// tables. It locates key/value pairs lexically and replaces or removes
// their bytes; callers validate the result with a real TOML parser.
package tomledit

import (
	"fmt"
	"regexp"
	"strings"
)

type entry struct {
	path      []string // full key path
	section   int      // index into headers, -1 for root
	keyPath   []string // key path as written (relative to section)
	lineStart int      // start of the line holding the key
	valStart  int
	valEnd    int
	lineEnd   int // just past the newline ending the entry (or len)
}

type header struct {
	path      []string
	lineStart int
	lineEnd   int
}

type scan struct {
	src     string
	pos     int
	entries []entry
	headers []header
}

// Doc is an editable TOML document.
type Doc struct{ src string }

// New wraps a TOML document.
func New(src []byte) *Doc { return &Doc{src: string(src)} }

// Bytes returns the current document.
func (d *Doc) Bytes() []byte { return []byte(d.src) }

func samePath(a, b []string) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}

func hasPrefix(path, prefix []string) bool {
	return len(path) >= len(prefix) && samePath(path[:len(prefix)], prefix)
}

// Set assigns literal (a TOML value as text, e.g. `"x"` or `true`) to the
// key at path, replacing the existing value or inserting a new line.
func (d *Doc) Set(path []string, literal string) error {
	s, err := parse(d.src)
	if err != nil {
		return err
	}
	for _, e := range s.entries {
		if samePath(e.path, path) {
			d.src = d.src[:e.valStart] + literal + d.src[e.valEnd:]
			return nil
		}
		if hasPrefix(path, e.path) {
			return fmt.Errorf("cannot set %s: %s is not a table here", strings.Join(path, "."), strings.Join(e.path, "."))
		}
	}
	parent, leaf := path[:len(path)-1], path[len(path)-1]

	// A [parent] table: insert after its last entry, or right after the header.
	for hi, h := range s.headers {
		if !samePath(h.path, parent) {
			continue
		}
		at := h.lineEnd
		for _, e := range s.entries {
			if e.section == hi {
				at = e.lineEnd
			}
		}
		d.insertLine(at, formatKey([]string{leaf})+" = "+literal)
		return nil
	}

	// Dotted keys under the parent in an enclosing section: keep that style.
	last := -1
	for i, e := range s.entries {
		sec := len(e.path) - len(e.keyPath)
		if len(parent) > 0 && sec <= len(parent) && len(e.path) > len(parent) && hasPrefix(e.path, parent) {
			last = i
		}
	}
	if last >= 0 {
		e := s.entries[last]
		sec := len(e.path) - len(e.keyPath)
		d.insertLine(e.lineEnd, formatKey(path[sec:])+" = "+literal)
		return nil
	}

	if len(parent) == 0 {
		at := -1
		for _, e := range s.entries {
			if e.section == -1 {
				at = e.lineEnd
			}
		}
		line := formatKey([]string{leaf}) + " = " + literal
		switch {
		case at >= 0:
			d.insertLine(at, line)
		case len(s.headers) > 0:
			d.src = d.src[:s.headers[0].lineStart] + line + "\n\n" + d.src[s.headers[0].lineStart:]
		default:
			d.insertLine(len(d.src), line)
		}
		return nil
	}

	// New table at the end of the document.
	var b strings.Builder
	b.WriteString(d.src)
	if len(d.src) > 0 && !strings.HasSuffix(d.src, "\n") {
		b.WriteByte('\n')
	}
	if len(strings.TrimSpace(d.src)) > 0 {
		b.WriteByte('\n')
	}
	b.WriteString("[" + formatKey(parent) + "]\n")
	b.WriteString(formatKey([]string{leaf}) + " = " + literal + "\n")
	d.src = b.String()
	return nil
}

// Unset removes the line(s) holding the key at path. It reports whether
// the key existed.
func (d *Doc) Unset(path []string) (bool, error) {
	s, err := parse(d.src)
	if err != nil {
		return false, err
	}
	for _, e := range s.entries {
		if samePath(e.path, path) {
			d.src = d.src[:e.lineStart] + d.src[e.lineEnd:]
			return true, nil
		}
	}
	return false, nil
}

func (d *Doc) insertLine(at int, line string) {
	prefix := d.src[:at]
	if len(prefix) > 0 && !strings.HasSuffix(prefix, "\n") {
		prefix += "\n"
	}
	d.src = prefix + line + "\n" + d.src[at:]
}

var bareKeyRe = regexp.MustCompile(`^[A-Za-z0-9_-]+$`)

func formatKey(path []string) string {
	parts := make([]string, len(path))
	for i, p := range path {
		if bareKeyRe.MatchString(p) {
			parts[i] = p
		} else {
			parts[i] = quoteKey(p)
		}
	}
	return strings.Join(parts, ".")
}

func quoteKey(s string) string {
	r := strings.NewReplacer(`\`, `\\`, `"`, `\"`, "\n", `\n`, "\t", `\t`)
	return `"` + r.Replace(s) + `"`
}

// --- scanner ---

func parse(src string) (*scan, error) {
	s := &scan{src: src}
	section := -1
	var sectionPath []string
	for s.pos < len(src) {
		s.skipWS()
		if s.eof() {
			break
		}
		switch c := s.peek(); {
		case c == '\n' || c == '\r':
			s.pos++
		case c == '#':
			s.skipToEOL()
		case c == '[':
			start := s.lineStart(s.pos)
			s.pos++
			array := false
			if !s.eof() && s.peek() == '[' {
				array = true
				s.pos++
			}
			s.skipWS()
			path, err := s.key()
			if err != nil {
				return nil, err
			}
			s.skipWS()
			if !s.consume("]") || (array && !s.consume("]")) {
				return nil, s.errorf("malformed table header")
			}
			s.skipToEOL()
			s.skipNewline()
			s.headers = append(s.headers, header{path: path, lineStart: start, lineEnd: s.pos})
			section = len(s.headers) - 1
			sectionPath = path
		default:
			start := s.lineStart(s.pos)
			kp, err := s.key()
			if err != nil {
				return nil, err
			}
			s.skipWS()
			if !s.consume("=") {
				return nil, s.errorf("expected '=' after key")
			}
			s.skipWS()
			valStart := s.pos
			if err := s.value(); err != nil {
				return nil, err
			}
			valEnd := s.pos
			s.skipToEOL()
			s.skipNewline()
			full := append(append([]string{}, sectionPath...), kp...)
			s.entries = append(s.entries, entry{
				path: full, section: section, keyPath: kp,
				lineStart: start, valStart: valStart, valEnd: valEnd, lineEnd: s.pos,
			})
		}
	}
	return s, nil
}

func (s *scan) eof() bool  { return s.pos >= len(s.src) }
func (s *scan) peek() byte { return s.src[s.pos] }

func (s *scan) errorf(format string, args ...any) error {
	line := strings.Count(s.src[:min(s.pos, len(s.src))], "\n") + 1
	return fmt.Errorf("toml line %d: %s", line, fmt.Sprintf(format, args...))
}

func (s *scan) lineStart(i int) int {
	return strings.LastIndexByte(s.src[:i], '\n') + 1
}

func (s *scan) skipWS() {
	for !s.eof() && (s.peek() == ' ' || s.peek() == '\t') {
		s.pos++
	}
}

func (s *scan) skipToEOL() {
	for !s.eof() && s.peek() != '\n' {
		s.pos++
	}
}

func (s *scan) skipNewline() {
	if !s.eof() && s.peek() == '\n' {
		s.pos++
	}
}

func (s *scan) consume(tok string) bool {
	if strings.HasPrefix(s.src[s.pos:], tok) {
		s.pos += len(tok)
		return true
	}
	return false
}

// key parses a (possibly dotted, possibly quoted) key.
func (s *scan) key() ([]string, error) {
	var parts []string
	for {
		s.skipWS()
		if s.eof() {
			return nil, s.errorf("expected key")
		}
		switch s.peek() {
		case '"':
			start := s.pos
			if err := s.basicString(); err != nil {
				return nil, err
			}
			parts = append(parts, unquoteBasic(s.src[start+1:s.pos-1]))
		case '\'':
			start := s.pos
			if err := s.literalString(); err != nil {
				return nil, err
			}
			parts = append(parts, s.src[start+1:s.pos-1])
		default:
			start := s.pos
			for !s.eof() && isBare(s.peek()) {
				s.pos++
			}
			if s.pos == start {
				return nil, s.errorf("expected key")
			}
			parts = append(parts, s.src[start:s.pos])
		}
		s.skipWS()
		if s.eof() || s.peek() != '.' {
			return parts, nil
		}
		s.pos++
	}
}

func isBare(c byte) bool {
	return c == '_' || c == '-' || (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9')
}

func unquoteBasic(s string) string {
	r := strings.NewReplacer(`\\`, `\`, `\"`, `"`, `\n`, "\n", `\t`, "\t", `\r`, "\r")
	return r.Replace(s)
}

// value advances past one TOML value.
func (s *scan) value() error {
	if s.eof() {
		return s.errorf("expected value")
	}
	switch s.peek() {
	case '"', '\'':
		return s.str()
	case '[', '{':
		depth := 0
		for !s.eof() {
			switch c := s.peek(); c {
			case '"', '\'':
				if err := s.str(); err != nil {
					return err
				}
				continue
			case '#':
				s.skipToEOL()
				continue
			case '[', '{':
				depth++
			case ']', '}':
				depth--
				if depth == 0 {
					s.pos++
					return nil
				}
			}
			s.pos++
		}
		return s.errorf("unterminated array or inline table")
	default:
		for !s.eof() {
			c := s.peek()
			if c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '#' {
				break
			}
			s.pos++
		}
		return nil
	}
}

func (s *scan) str() error {
	switch {
	case strings.HasPrefix(s.src[s.pos:], `"""`):
		s.pos += 3
		for !s.eof() {
			if s.peek() == '\\' {
				s.pos += 2
				continue
			}
			if s.consume(`"""`) {
				for i := 0; i < 2 && !s.eof() && s.peek() == '"'; i++ {
					s.pos++
				}
				return nil
			}
			s.pos++
		}
		return s.errorf("unterminated multi-line string")
	case strings.HasPrefix(s.src[s.pos:], `'''`):
		s.pos += 3
		end := strings.Index(s.src[s.pos:], `'''`)
		if end < 0 {
			return s.errorf("unterminated multi-line string")
		}
		s.pos += end + 3
		for i := 0; i < 2 && !s.eof() && s.peek() == '\''; i++ {
			s.pos++
		}
		return nil
	case s.peek() == '"':
		return s.basicString()
	default:
		return s.literalString()
	}
}

func (s *scan) basicString() error {
	s.pos++
	for !s.eof() {
		switch s.peek() {
		case '\\':
			s.pos += 2
			continue
		case '"':
			s.pos++
			return nil
		case '\n':
			return s.errorf("unterminated string")
		}
		s.pos++
	}
	return s.errorf("unterminated string")
}

func (s *scan) literalString() error {
	s.pos++
	for !s.eof() {
		switch s.peek() {
		case '\'':
			s.pos++
			return nil
		case '\n':
			return s.errorf("unterminated string")
		}
		s.pos++
	}
	return s.errorf("unterminated string")
}
