package launchd

import "testing"

const printRunning = `gui/501/hum.t3 = {
	active count = 1
	path = /Users/me/Library/LaunchAgents/hum.t3.plist
	type = LaunchAgent
	state = running

	program = /opt/homebrew/bin/hum
	arguments = {
		/opt/homebrew/bin/hum
		run
		t3
	}

	pid = 4242
	immediate reason = speculative
	forks = 0
	last exit code = (never exited)
	environment = {
		state = nested-should-be-ignored
	}
}
`

const printCrashed = `gui/501/hum.api = {
	active count = 0
	path = /Users/me/Library/LaunchAgents/hum.api.plist
	state = not running
	runs = 3
	last exit code = 78: EX_CONFIG
}
`

func TestParsePrint(t *testing.T) {
	st := ParsePrint(printRunning)
	if !st.Loaded || !st.Running || st.PID != 4242 || st.LastExit != nil ||
		st.Path != "/Users/me/Library/LaunchAgents/hum.t3.plist" {
		t.Fatalf("%+v", st)
	}
	st = ParsePrint(printCrashed)
	if st.Running || st.LastExit == nil || *st.LastExit != 78 || !st.Crashed() {
		t.Fatalf("%+v", st)
	}
	st = ParsePrint("gui/501/x = {\n\tstate = not running\n\tlast terminating signal = Killed: 9\n}\n")
	if !st.Crashed() || st.LastSignal != "Killed: 9" {
		t.Fatalf("%+v", st)
	}
}

func TestParseDisabled(t *testing.T) {
	out := `disabled services = {
	"hum.t3" => disabled
	"hum.api" => enabled
	"com.old" => true
	"com.older" => false
}
`
	d := ParseDisabled(out)
	if !d["hum.t3"] || d["hum.api"] || !d["com.old"] || d["com.older"] {
		t.Fatalf("%v", d)
	}
}
