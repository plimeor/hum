// Package dotenv parses .env files with forward-only variable expansion.
//
// Rules:
//   - KEY=VALUE per line, optional "export " prefix, "#" starts a comment.
//   - Unquoted values are trimmed; " #" starts an inline comment.
//   - '…' values are literal and are never expanded.
//   - "…" values support \n \r \t \" \\ \$ escapes and may span lines.
//   - ${VAR} and $VAR expand in unquoted and double-quoted values, resolved
//     against the lookup (lower-priority layers) and lines above in the
//     same file. Undefined variables expand to "" and are reported.
//   - \$ yields a literal "$".
//
// godotenv differs in ways that matter here: it only resolves variables
// defined earlier in the same file and only matches upper-case names, so
// hum does not use it.
package dotenv

import (
	"fmt"
	"strings"
)

// KV is one assignment.
type KV struct {
	Key   string
	Value string
	Line  int
}

// Undefined is a ${VAR} reference that resolved to nothing.
type Undefined struct {
	Line int
	Name string
}

// Lookup resolves a variable from lower-priority layers.
type Lookup func(name string) (string, bool)

// Result is the outcome of Parse.
type Result struct {
	Vars      []KV
	Undefined []Undefined
}

type parser struct {
	src    []byte
	pos    int
	line   int
	lookup Lookup
	local  map[string]string
	res    Result
}

// Parse parses data. lookup may be nil.
func Parse(data []byte, lookup Lookup) (*Result, error) {
	p := &parser{src: data, line: 1, lookup: lookup, local: map[string]string{}}
	if err := p.run(); err != nil {
		return nil, err
	}
	return &p.res, nil
}

func (p *parser) eof() bool  { return p.pos >= len(p.src) }
func (p *parser) peek() byte { return p.src[p.pos] }

func (p *parser) errorf(format string, args ...any) error {
	return fmt.Errorf("line %d: %s", p.line, fmt.Sprintf(format, args...))
}

func (p *parser) skipInline() {
	for !p.eof() && (p.peek() == ' ' || p.peek() == '\t') {
		p.pos++
	}
}

func (p *parser) skipToEOL() {
	for !p.eof() && p.peek() != '\n' {
		p.pos++
	}
}

// endLine consumes the rest of a line after a value: whitespace and an
// optional comment.
func (p *parser) endLine() error {
	p.skipInline()
	if !p.eof() && p.peek() == '\r' {
		p.pos++
	}
	if p.eof() {
		return nil
	}
	switch p.peek() {
	case '\n':
		return nil
	case '#':
		p.skipToEOL()
		return nil
	}
	return p.errorf("unexpected text after value")
}

func isKeyStart(c byte) bool {
	return c == '_' || (c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z')
}

func isKeyChar(c byte) bool { return isKeyStart(c) || (c >= '0' && c <= '9') || c == '.' }

func isNameChar(c byte) bool { return isKeyStart(c) || (c >= '0' && c <= '9') }

func (p *parser) readKey() string {
	start := p.pos
	if p.eof() || !isKeyStart(p.peek()) {
		return ""
	}
	for !p.eof() && isKeyChar(p.peek()) {
		p.pos++
	}
	return string(p.src[start:p.pos])
}

func (p *parser) run() error {
	for {
		// Skip blank lines and comments.
		for !p.eof() {
			c := p.peek()
			if c == ' ' || c == '\t' || c == '\r' {
				p.pos++
			} else if c == '\n' {
				p.pos++
				p.line++
			} else if c == '#' {
				p.skipToEOL()
			} else {
				break
			}
		}
		if p.eof() {
			return nil
		}
		line := p.line
		key := p.readKey()
		if key == "export" && !p.eof() && (p.peek() == ' ' || p.peek() == '\t') {
			p.skipInline()
			key = p.readKey()
		}
		if key == "" {
			return p.errorf("expected a variable name")
		}
		p.skipInline()
		if p.eof() || p.peek() != '=' {
			return p.errorf("expected '=' after %s", key)
		}
		p.pos++
		p.skipInline()
		val, err := p.readValue(line)
		if err != nil {
			return err
		}
		if err := p.endLine(); err != nil {
			return err
		}
		p.local[key] = val
		p.res.Vars = append(p.res.Vars, KV{Key: key, Value: val, Line: line})
	}
}

func (p *parser) readValue(line int) (string, error) {
	if p.eof() {
		return "", nil
	}
	switch p.peek() {
	case '\'':
		p.pos++
		start := p.pos
		for !p.eof() && p.peek() != '\'' {
			if p.peek() == '\n' {
				p.line++
			}
			p.pos++
		}
		if p.eof() {
			return "", fmt.Errorf("line %d: unterminated single-quoted value", line)
		}
		v := string(p.src[start:p.pos])
		p.pos++
		return v, nil
	case '"':
		p.pos++
		var b strings.Builder
		for {
			if p.eof() {
				return "", fmt.Errorf("line %d: unterminated double-quoted value", line)
			}
			c := p.peek()
			switch {
			case c == '"':
				p.pos++
				return b.String(), nil
			case c == '\\' && p.pos+1 < len(p.src):
				p.pos++
				e := p.peek()
				p.pos++
				switch e {
				case 'n':
					b.WriteByte('\n')
				case 'r':
					b.WriteByte('\r')
				case 't':
					b.WriteByte('\t')
				default:
					// \" \\ \$ and anything else: the character itself.
					if e == '\n' {
						p.line++
					}
					b.WriteByte(e)
				}
			case c == '$':
				p.expand(&b, line)
			default:
				if c == '\n' {
					p.line++
				}
				b.WriteByte(c)
				p.pos++
			}
		}
	default:
		// Unquoted: up to end of line, " #" starts a comment.
		start := p.pos
		end := start
		for end < len(p.src) && p.src[end] != '\n' {
			if p.src[end] == '#' && end > start && (p.src[end-1] == ' ' || p.src[end-1] == '\t') {
				break
			}
			end++
		}
		raw := strings.TrimRight(string(p.src[start:end]), " \t\r")
		p.pos = start + len(raw)
		sub := &parser{src: []byte(raw), line: line, lookup: p.lookup, local: p.local}
		var b strings.Builder
		for !sub.eof() {
			c := sub.peek()
			if c == '\\' && sub.pos+1 < len(sub.src) && sub.src[sub.pos+1] == '$' {
				b.WriteByte('$')
				sub.pos += 2
			} else if c == '$' {
				sub.expand(&b, line)
			} else {
				b.WriteByte(c)
				sub.pos++
			}
		}
		p.res.Undefined = append(p.res.Undefined, sub.res.Undefined...)
		return b.String(), nil
	}
}

// expand handles a "$" at p.pos, writing the expansion (or the literal
// text when it is not a valid reference).
func (p *parser) expand(b *strings.Builder, line int) {
	p.pos++ // '$'
	if !p.eof() && p.peek() == '{' {
		end := p.pos + 1
		for end < len(p.src) && isNameChar(p.src[end]) {
			end++
		}
		name := string(p.src[p.pos+1 : end])
		if end < len(p.src) && p.src[end] == '}' && name != "" && isKeyStart(name[0]) {
			p.pos = end + 1
			b.WriteString(p.resolve(name, line))
			return
		}
		b.WriteByte('$')
		return
	}
	if !p.eof() && isKeyStart(p.peek()) {
		start := p.pos
		for !p.eof() && isNameChar(p.peek()) {
			p.pos++
		}
		b.WriteString(p.resolve(string(p.src[start:p.pos]), line))
		return
	}
	b.WriteByte('$')
}

func (p *parser) resolve(name string, line int) string {
	if v, ok := p.local[name]; ok {
		return v
	}
	if p.lookup != nil {
		if v, ok := p.lookup(name); ok {
			return v
		}
	}
	p.res.Undefined = append(p.res.Undefined, Undefined{Line: line, Name: name})
	return ""
}
