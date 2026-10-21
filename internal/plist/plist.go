// Package plist renders launchd property lists. Output is deterministic so
// that comparing bytes tells whether a plist on disk is current.
package plist

import (
	"bytes"
	"encoding/xml"
	"regexp"
	"sort"
	"strconv"
)

// KeepAlive modes.
const (
	KeepAliveNone      = ""           // no KeepAlive key
	KeepAliveAlways    = "always"     // KeepAlive = true
	KeepAliveOnFailure = "on-failure" // KeepAlive = { SuccessfulExit = false }
)

// Job describes one launchd job.
type Job struct {
	Label            string
	ProgramArguments []string
	RunAtLoad        bool
	KeepAlive        string
	LogPath          string // StandardOutPath and StandardErrorPath
	Env              map[string]string
	StartInterval    int // seconds; 0 = unset
}

// Render returns the plist XML.
func Render(j Job) []byte {
	var b bytes.Buffer
	b.WriteString(`<?xml version="1.0" encoding="UTF-8"?>` + "\n")
	b.WriteString(`<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">` + "\n")
	b.WriteString(`<plist version="1.0">` + "\n<dict>\n")
	key := func(k string) { b.WriteString("\t<key>" + esc(k) + "</key>\n") }
	str := func(indent, s string) { b.WriteString(indent + "<string>" + esc(s) + "</string>\n") }
	boolean := func(indent string, v bool) {
		if v {
			b.WriteString(indent + "<true/>\n")
		} else {
			b.WriteString(indent + "<false/>\n")
		}
	}

	key("Label")
	str("\t", j.Label)
	key("ProgramArguments")
	b.WriteString("\t<array>\n")
	for _, a := range j.ProgramArguments {
		str("\t\t", a)
	}
	b.WriteString("\t</array>\n")
	if len(j.Env) > 0 {
		key("EnvironmentVariables")
		b.WriteString("\t<dict>\n")
		keys := make([]string, 0, len(j.Env))
		for k := range j.Env {
			keys = append(keys, k)
		}
		sort.Strings(keys)
		for _, k := range keys {
			b.WriteString("\t\t<key>" + esc(k) + "</key>\n")
			str("\t\t", j.Env[k])
		}
		b.WriteString("\t</dict>\n")
	}
	key("RunAtLoad")
	boolean("\t", j.RunAtLoad)
	switch j.KeepAlive {
	case KeepAliveAlways:
		key("KeepAlive")
		boolean("\t", true)
	case KeepAliveOnFailure:
		key("KeepAlive")
		b.WriteString("\t<dict>\n\t\t<key>SuccessfulExit</key>\n")
		boolean("\t\t", false)
		b.WriteString("\t</dict>\n")
	}
	if j.StartInterval > 0 {
		key("StartInterval")
		b.WriteString("\t<integer>" + strconv.Itoa(j.StartInterval) + "</integer>\n")
	}
	if j.LogPath != "" {
		key("StandardOutPath")
		str("\t", j.LogPath)
		key("StandardErrorPath")
		str("\t", j.LogPath)
	}
	b.WriteString("</dict>\n</plist>\n")
	return b.Bytes()
}

func esc(s string) string {
	var b bytes.Buffer
	_ = xml.EscapeText(&b, []byte(s))
	return b.String()
}

var programRe = regexp.MustCompile(`(?s)<key>ProgramArguments</key>\s*<array>\s*<string>(.*?)</string>`)

// Program extracts ProgramArguments[0] from a plist rendered by Render.
func Program(data []byte) (string, bool) {
	m := programRe.FindSubmatch(data)
	if m == nil {
		return "", false
	}
	var out string
	if err := xml.Unmarshal([]byte("<s>"+string(m[1])+"</s>"), &out); err != nil {
		return string(m[1]), true
	}
	return out, true
}
