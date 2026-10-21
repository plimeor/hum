package dotenv

import "testing"

func parse(t *testing.T, src string, lower map[string]string) (map[string]string, *Result) {
	t.Helper()
	res, err := Parse([]byte(src), func(k string) (string, bool) { v, ok := lower[k]; return v, ok })
	if err != nil {
		t.Fatal(err)
	}
	m := map[string]string{}
	for _, kv := range res.Vars {
		m[kv.Key] = kv.Value
	}
	return m, res
}

func TestParseForms(t *testing.T) {
	src := `# comment
PLAIN=hello world   # inline comment
export EXPORTED=1
SINGLE='no $HOME expansion # kept'
DOUBLE="line1\nline2 \"q\" \$HOME"
HASH=a#b
EMPTY=
SPACED = value
MULTI="a
b"
`
	m, _ := parse(t, src, map[string]string{"HOME": "/h"})
	want := map[string]string{
		"PLAIN":    "hello world",
		"EXPORTED": "1",
		"SINGLE":   "no $HOME expansion # kept",
		"DOUBLE":   "line1\nline2 \"q\" $HOME",
		"HASH":     "a#b",
		"EMPTY":    "",
		"SPACED":   "value",
		"MULTI":    "a\nb",
	}
	for k, v := range want {
		if m[k] != v {
			t.Errorf("%s = %q, want %q", k, m[k], v)
		}
	}
}

func TestExpansion(t *testing.T) {
	src := `DATA_DIR=${HOME}/data
URL="http://$HOST:${PORT}/x"
PORT=8080
LATE=${PORT}
ESC=\$HOME
LOWER=$lower_var
DOLLAR=cost $5 and $ alone and ${unterminated
`
	m, res := parse(t, src, map[string]string{"HOME": "/Users/me", "HOST": "localhost", "lower_var": "ok"})
	checks := map[string]string{
		"DATA_DIR": "/Users/me/data",
		"URL":      "http://localhost:/x", // PORT is defined later: forward-only
		"LATE":     "8080",
		"ESC":      "$HOME",
		"LOWER":    "ok",
		"DOLLAR":   "cost $5 and $ alone and ${unterminated",
	}
	for k, v := range checks {
		if m[k] != v {
			t.Errorf("%s = %q, want %q", k, m[k], v)
		}
	}
	if len(res.Undefined) != 1 || res.Undefined[0].Name != "PORT" || res.Undefined[0].Line != 2 {
		t.Errorf("undefined = %+v", res.Undefined)
	}
}

func TestSameFileOverridesLower(t *testing.T) {
	m, _ := parse(t, "PATH=/bin\nP2=$PATH:/x\n", map[string]string{"PATH": "/usr/bin"})
	if m["P2"] != "/bin:/x" {
		t.Errorf("P2 = %q", m["P2"])
	}
}

func TestParseErrors(t *testing.T) {
	for _, src := range []string{"NOEQUALS\n", "A='unterminated\n", "A=\"unterminated\n", "1A=x\n", "A='x' junk\n"} {
		if _, err := Parse([]byte(src), nil); err == nil {
			t.Errorf("%q: expected error", src)
		}
	}
}

func TestCRLF(t *testing.T) {
	m, _ := parse(t, "A=1\r\nB=\"2\"\r\n", nil)
	if m["A"] != "1" || m["B"] != "2" {
		t.Errorf("%q %q", m["A"], m["B"])
	}
}
