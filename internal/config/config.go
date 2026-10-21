package config

import (
	"bytes"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strconv"
	"strings"

	"github.com/pelletier/go-toml/v2"
)

// Restart policies.
const (
	RestartAlways    = "always"
	RestartOnFailure = "on-failure"
	RestartNever     = "never"
)

// Log rotation defaults used when neither the global nor the service
// config sets a value.
const (
	DefaultLogMaxSize = "10MB"
	DefaultLogKeep    = 3
)

var envKeyRe = regexp.MustCompile(`^[A-Za-z_][A-Za-z0-9_]*$`)

// ValidateEnvKey reports whether key is a usable environment variable name.
func ValidateEnvKey(key string) error {
	if !envKeyRe.MatchString(key) {
		return fmt.Errorf("invalid environment variable name %q", key)
	}
	return nil
}

// LogSettings is the [log] table, in the global or a service config.
type LogSettings struct {
	MaxSize string `toml:"max_size"`
	Keep    *int   `toml:"keep"`
}

// Service is a parsed services/<name>.toml.
type Service struct {
	Name string
	// Command is the argv form; Shell is set instead when the config gives
	// a string, which runs through /bin/sh -c.
	Command   []string
	Shell     string
	Cwd       string // as written; empty means $HOME
	EnvFile   []string
	Autostart bool
	Restart   string
	Env       map[string]string
	Log       LogSettings
}

type rawService struct {
	Command   any               `toml:"command"`
	Cwd       string            `toml:"cwd"`
	EnvFile   []string          `toml:"env_file"`
	Autostart *bool             `toml:"autostart"`
	Restart   string            `toml:"restart"`
	Env       map[string]string `toml:"env"`
	Log       LogSettings       `toml:"log"`
}

// Argv returns the program and arguments to exec.
func (s *Service) Argv() []string {
	if s.Shell != "" {
		return []string{"/bin/sh", "-c", s.Shell}
	}
	return s.Command
}

// CommandString renders the command for display.
func (s *Service) CommandString() string {
	if s.Shell != "" {
		return s.Shell
	}
	parts := make([]string, len(s.Command))
	for i, a := range s.Command {
		if a == "" || strings.ContainsAny(a, " \t\n\"'\\$") {
			parts[i] = strconv.Quote(a)
		} else {
			parts[i] = a
		}
	}
	return strings.Join(parts, " ")
}

// ResolvedCwd returns the absolute working directory.
func (s *Service) ResolvedCwd(home string) string {
	if s.Cwd == "" {
		return home
	}
	return ExpandPath(s.Cwd, home)
}

// ResolvedEnvFiles returns env_file entries expanded and made absolute
// relative to the working directory.
func (s *Service) ResolvedEnvFiles(home string) []string {
	cwd := s.ResolvedCwd(home)
	out := make([]string, len(s.EnvFile))
	for i, f := range s.EnvFile {
		f = ExpandPath(f, home)
		if !filepath.IsAbs(f) {
			f = filepath.Join(cwd, f)
		}
		out[i] = f
	}
	return out
}

func decodeStrict(data []byte, v any) error {
	dec := toml.NewDecoder(bytes.NewReader(data))
	dec.DisallowUnknownFields()
	if err := dec.Decode(v); err != nil {
		var strict *toml.StrictMissingError
		if errors.As(err, &strict) {
			keys := make([]string, len(strict.Errors))
			for i, e := range strict.Errors {
				keys[i] = strings.Join(e.Key(), ".")
			}
			return fmt.Errorf("unknown field(s): %s", strings.Join(keys, ", "))
		}
		var de *toml.DecodeError
		if errors.As(err, &de) {
			row, col := de.Position()
			return fmt.Errorf("line %d, column %d: %s", row, col, de.Error())
		}
		return err
	}
	return nil
}

// ParseService parses and validates a service config.
func ParseService(name string, data []byte) (*Service, error) {
	var raw rawService
	if err := decodeStrict(data, &raw); err != nil {
		return nil, err
	}
	s := &Service{
		Name:      name,
		Cwd:       raw.Cwd,
		EnvFile:   raw.EnvFile,
		Autostart: true,
		Restart:   RestartAlways,
		Env:       raw.Env,
		Log:       raw.Log,
	}
	switch c := raw.Command.(type) {
	case nil:
		return nil, errors.New("command is required")
	case string:
		if strings.TrimSpace(c) == "" {
			return nil, errors.New("command must not be empty")
		}
		s.Shell = c
	case []any:
		if len(c) == 0 {
			return nil, errors.New("command must not be empty")
		}
		for i, a := range c {
			str, ok := a.(string)
			if !ok {
				return nil, fmt.Errorf("command[%d] must be a string", i)
			}
			s.Command = append(s.Command, str)
		}
		if s.Command[0] == "" {
			return nil, errors.New("command[0] must not be empty")
		}
	default:
		return nil, errors.New("command must be an array of strings or a string")
	}
	if raw.Autostart != nil {
		s.Autostart = *raw.Autostart
	}
	if raw.Restart != "" {
		s.Restart = raw.Restart
	}
	switch s.Restart {
	case RestartAlways, RestartOnFailure, RestartNever:
	default:
		return nil, fmt.Errorf("restart must be one of always, on-failure, never (got %q)", s.Restart)
	}
	for i, f := range s.EnvFile {
		if f == "" {
			return nil, fmt.Errorf("env_file[%d] must not be empty", i)
		}
	}
	if err := validateEnv(s.Env); err != nil {
		return nil, err
	}
	if err := validateLog(s.Log); err != nil {
		return nil, err
	}
	return s, nil
}

// LoadService reads and parses services/<name>.toml.
func LoadService(p Paths, name string) (*Service, error) {
	if err := ValidateName(name); err != nil {
		return nil, err
	}
	data, err := os.ReadFile(p.ServiceFile(name))
	if os.IsNotExist(err) {
		return nil, fmt.Errorf("service %q not found (no %s)", name, p.ServiceFile(name))
	}
	if err != nil {
		return nil, err
	}
	s, err := ParseService(name, data)
	if err != nil {
		return nil, fmt.Errorf("%s: %w", p.ServiceFile(name), err)
	}
	return s, nil
}

// Global is the parsed config.toml.
type Global struct {
	Env map[string]string `toml:"env"`
	Log LogSettings       `toml:"log"`
}

// ParseGlobal parses and validates config.toml.
func ParseGlobal(data []byte) (*Global, error) {
	var g Global
	if err := decodeStrict(data, &g); err != nil {
		return nil, err
	}
	if err := validateEnv(g.Env); err != nil {
		return nil, err
	}
	if err := validateLog(g.Log); err != nil {
		return nil, err
	}
	return &g, nil
}

// LoadGlobal reads config.toml; a missing file is an empty config.
func LoadGlobal(p Paths) (*Global, error) {
	data, err := os.ReadFile(p.GlobalFile())
	if os.IsNotExist(err) {
		return &Global{}, nil
	}
	if err != nil {
		return nil, err
	}
	g, err := ParseGlobal(data)
	if err != nil {
		return nil, fmt.Errorf("%s: %w", p.GlobalFile(), err)
	}
	return g, nil
}

// EnsureGlobal creates config.toml with the given PATH when it does not
// exist yet. It reports whether the file was created.
func EnsureGlobal(p Paths, path string) (bool, error) {
	if _, err := os.Stat(p.GlobalFile()); err == nil {
		return false, nil
	} else if !os.IsNotExist(err) {
		return false, err
	}
	var b strings.Builder
	b.WriteString("# hum global config, shared by every service.\n")
	b.WriteString("# PATH was captured from the shell that first ran hum.\n\n")
	b.WriteString("[env]\n")
	if path != "" {
		fmt.Fprintf(&b, "PATH = %s\n", Quote(path))
	}
	b.WriteString("\n[log]\n")
	fmt.Fprintf(&b, "# max_size = %s\n", Quote(DefaultLogMaxSize))
	fmt.Fprintf(&b, "# keep = %d\n", DefaultLogKeep)
	if err := os.MkdirAll(p.ServicesDir(), 0o755); err != nil {
		return false, err
	}
	return true, WriteFileAtomic(p.GlobalFile(), []byte(b.String()), 0o644)
}

// EffectiveLog resolves rotation settings: service over global over default.
func EffectiveLog(g *Global, s *Service) (maxSize int64, keep int) {
	sizeStr, keep := DefaultLogMaxSize, DefaultLogKeep
	if g != nil {
		if g.Log.MaxSize != "" {
			sizeStr = g.Log.MaxSize
		}
		if g.Log.Keep != nil {
			keep = *g.Log.Keep
		}
	}
	if s != nil {
		if s.Log.MaxSize != "" {
			sizeStr = s.Log.MaxSize
		}
		if s.Log.Keep != nil {
			keep = *s.Log.Keep
		}
	}
	maxSize, _ = ParseSize(sizeStr) // validated on load
	return maxSize, keep
}

func validateEnv(env map[string]string) error {
	keys := make([]string, 0, len(env))
	for k := range env {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	for _, k := range keys {
		if err := ValidateEnvKey(k); err != nil {
			return fmt.Errorf("[env]: %w", err)
		}
	}
	return nil
}

func validateLog(l LogSettings) error {
	if l.MaxSize != "" {
		if _, err := ParseSize(l.MaxSize); err != nil {
			return fmt.Errorf("[log] max_size: %w", err)
		}
	}
	if l.Keep != nil && *l.Keep < 0 {
		return errors.New("[log] keep must be >= 0")
	}
	return nil
}

var sizeRe = regexp.MustCompile(`^(\d+)\s*([KMG]?)I?B?$`)

// ParseSize parses sizes such as "10MB", "512KB", "1G" or "4096".
// Units are powers of 1024.
func ParseSize(s string) (int64, error) {
	m := sizeRe.FindStringSubmatch(strings.ToUpper(strings.TrimSpace(s)))
	if m == nil {
		return 0, fmt.Errorf("invalid size %q (e.g. \"10MB\")", s)
	}
	n, err := strconv.ParseInt(m[1], 10, 64)
	if err != nil {
		return 0, err
	}
	switch m[2] {
	case "K":
		n <<= 10
	case "M":
		n <<= 20
	case "G":
		n <<= 30
	}
	if n <= 0 {
		return 0, fmt.Errorf("size %q must be positive", s)
	}
	return n, nil
}

// Quote renders s as a TOML basic string.
func Quote(s string) string {
	var b strings.Builder
	b.WriteByte('"')
	for _, r := range s {
		switch r {
		case '"':
			b.WriteString(`\"`)
		case '\\':
			b.WriteString(`\\`)
		case '\n':
			b.WriteString(`\n`)
		case '\t':
			b.WriteString(`\t`)
		case '\r':
			b.WriteString(`\r`)
		default:
			if r < 0x20 || r == 0x7f {
				fmt.Fprintf(&b, `\u%04X`, r)
			} else {
				b.WriteRune(r)
			}
		}
	}
	b.WriteByte('"')
	return b.String()
}

// QuoteArray renders a TOML inline array of strings.
func QuoteArray(items []string) string {
	parts := make([]string, len(items))
	for i, it := range items {
		parts[i] = Quote(it)
	}
	return "[" + strings.Join(parts, ", ") + "]"
}

// RenderService renders a freshly created service config. Only fields with
// a value are written; autostart and restart are always explicit.
func RenderService(s *Service) []byte {
	var b strings.Builder
	if s.Shell != "" {
		fmt.Fprintf(&b, "command = %s\n", Quote(s.Shell))
	} else {
		fmt.Fprintf(&b, "command = %s\n", QuoteArray(s.Command))
	}
	if s.Cwd != "" {
		fmt.Fprintf(&b, "cwd = %s\n", Quote(s.Cwd))
	}
	if len(s.EnvFile) > 0 {
		fmt.Fprintf(&b, "env_file = %s\n", QuoteArray(s.EnvFile))
	}
	fmt.Fprintf(&b, "autostart = %t\n", s.Autostart)
	fmt.Fprintf(&b, "restart = %s\n", Quote(s.Restart))
	if len(s.Env) > 0 {
		keys := make([]string, 0, len(s.Env))
		for k := range s.Env {
			keys = append(keys, k)
		}
		sort.Strings(keys)
		b.WriteString("\n[env]\n")
		for _, k := range keys {
			fmt.Fprintf(&b, "%s = %s\n", k, Quote(s.Env[k]))
		}
	}
	return []byte(b.String())
}
