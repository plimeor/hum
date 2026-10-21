package tomledit

import "testing"

const sample = `# ~/.config/hum/services/t3.toml
command = ["t3", "server"]   # argv
cwd = "~/work/t3"
autostart = true             # login
restart = "always"           # policy

[env]
PORT = "3773"  # avoid 3000
DEBUG = "1"
`

func check(t *testing.T, got []byte, want string) {
	t.Helper()
	if string(got) != want {
		t.Fatalf("got:\n%s\nwant:\n%s", got, want)
	}
}

func TestSetReplaceKeepsComments(t *testing.T) {
	d := New([]byte(sample))
	if err := d.Set([]string{"env", "PORT"}, `"4000"`); err != nil {
		t.Fatal(err)
	}
	if err := d.Set([]string{"restart"}, `"on-failure"`); err != nil {
		t.Fatal(err)
	}
	check(t, d.Bytes(), `# ~/.config/hum/services/t3.toml
command = ["t3", "server"]   # argv
cwd = "~/work/t3"
autostart = true             # login
restart = "on-failure"           # policy

[env]
PORT = "4000"  # avoid 3000
DEBUG = "1"
`)
}

func TestSetInsert(t *testing.T) {
	d := New([]byte(sample))
	if err := d.Set([]string{"env", "NEW"}, `"x"`); err != nil {
		t.Fatal(err)
	}
	if err := d.Set([]string{"env_file"}, `[".env"]`); err != nil {
		t.Fatal(err)
	}
	if err := d.Set([]string{"log", "keep"}, `5`); err != nil {
		t.Fatal(err)
	}
	check(t, d.Bytes(), `# ~/.config/hum/services/t3.toml
command = ["t3", "server"]   # argv
cwd = "~/work/t3"
autostart = true             # login
restart = "always"           # policy
env_file = [".env"]

[env]
PORT = "3773"  # avoid 3000
DEBUG = "1"
NEW = "x"

[log]
keep = 5
`)
}

func TestUnset(t *testing.T) {
	d := New([]byte(sample))
	found, err := d.Unset([]string{"env", "DEBUG"})
	if err != nil || !found {
		t.Fatal(found, err)
	}
	found, _ = d.Unset([]string{"env", "MISSING"})
	if found {
		t.Fatal("missing key reported as found")
	}
	if _, err := d.Unset([]string{"autostart"}); err != nil {
		t.Fatal(err)
	}
	check(t, d.Bytes(), `# ~/.config/hum/services/t3.toml
command = ["t3", "server"]   # argv
cwd = "~/work/t3"
restart = "always"           # policy

[env]
PORT = "3773"  # avoid 3000
`)
}

func TestMultilineArray(t *testing.T) {
	src := "command = [\n  \"t3\", # program\n  \"server]\",\n]  # trailing\ncwd = \"/x\"\n"
	d := New([]byte(src))
	if err := d.Set([]string{"command"}, `["a"]`); err != nil {
		t.Fatal(err)
	}
	check(t, d.Bytes(), "command = [\"a\"]  # trailing\ncwd = \"/x\"\n")
}

func TestDottedAndQuotedKeys(t *testing.T) {
	d := New([]byte("command = [\"a\"]\nenv.A = \"1\"\n\"env\".\"B\" = '2'\n"))
	if err := d.Set([]string{"env", "B"}, `"3"`); err != nil {
		t.Fatal(err)
	}
	if err := d.Set([]string{"env", "C"}, `"4"`); err != nil {
		t.Fatal(err)
	}
	check(t, d.Bytes(), "command = [\"a\"]\nenv.A = \"1\"\n\"env\".\"B\" = \"3\"\nenv.C = \"4\"\n")
}

func TestRootInsertBeforeFirstTable(t *testing.T) {
	d := New([]byte("[env]\nA = \"1\"\n"))
	if err := d.Set([]string{"command"}, `["a"]`); err != nil {
		t.Fatal(err)
	}
	check(t, d.Bytes(), "command = [\"a\"]\n\n[env]\nA = \"1\"\n")
}

func TestNoTrailingNewline(t *testing.T) {
	d := New([]byte(`command = ["a"]`))
	if err := d.Set([]string{"cwd"}, `"/x"`); err != nil {
		t.Fatal(err)
	}
	check(t, d.Bytes(), "command = [\"a\"]\ncwd = \"/x\"\n")
}

func TestInlineTableRefused(t *testing.T) {
	d := New([]byte("env = { A = \"1\" }\n"))
	if err := d.Set([]string{"env", "B"}, `"2"`); err == nil {
		t.Fatal("expected error for inline table")
	}
}

func TestMultilineStringValue(t *testing.T) {
	d := New([]byte("command = \"\"\"\necho a\n# not a comment\n\"\"\"\ncwd = \"/x\"\n"))
	if err := d.Set([]string{"cwd"}, `"/y"`); err != nil {
		t.Fatal(err)
	}
	check(t, d.Bytes(), "command = \"\"\"\necho a\n# not a comment\n\"\"\"\ncwd = \"/y\"\n")
}
