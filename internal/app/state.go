package app

import (
	"fmt"
	"os"

	"github.com/plimeor/hum/internal/config"
	"github.com/plimeor/hum/internal/launchd"
)

// States shown by `hum ls`.
const (
	StateRunning  = "running"
	StateStopped  = "stopped"
	StateCrashed  = "crashed"
	StateDisabled = "disabled"
)

// ServiceState is one row of `hum ls`.
type ServiceState struct {
	Name       string `json:"name"`
	State      string `json:"state"`
	PID        int    `json:"pid,omitempty"`
	LastExit   *int   `json:"last_exit,omitempty"`
	LastSignal string `json:"last_signal,omitempty"`
	Command    string `json:"command,omitempty"`
	// OutOfSync is set when launchd does not match the config directory;
	// Reason says how.
	OutOfSync bool   `json:"out_of_sync"`
	Reason    string `json:"reason,omitempty"`
	Error     string `json:"error,omitempty"`
}

// StateOf derives the displayed state.
func StateOf(st launchd.Status, disabled bool) string {
	switch {
	case st.Running:
		return StateRunning
	case disabled:
		return StateDisabled
	case st.Crashed():
		return StateCrashed
	default:
		return StateStopped
	}
}

// States reports every service with a config file or a plist.
func (a *App) States() ([]ServiceState, error) {
	cfgNames, err := a.Paths.ServiceNames()
	if err != nil {
		return nil, err
	}
	plistNames, err := a.Paths.PlistNames()
	if err != nil {
		return nil, err
	}
	disabled, err := a.LD.Disabled()
	if err != nil {
		return nil, err
	}
	seen := map[string]bool{}
	var rows []ServiceState
	add := func(name string, s *config.Service, loadErr error) error {
		label := config.Label(name)
		st, err := a.LD.Status(label)
		if err != nil {
			return err
		}
		row := ServiceState{
			Name:       name,
			State:      StateOf(st, disabled[label]),
			PID:        st.PID,
			LastExit:   st.LastExit,
			LastSignal: st.LastSignal,
		}
		switch {
		case loadErr != nil:
			row.Error = loadErr.Error()
		case s == nil:
			row.OutOfSync, row.Reason = true, "plist without config"
		default:
			row.Command = s.CommandString()
			row.OutOfSync, row.Reason = a.syncReason(s, st)
		}
		rows = append(rows, row)
		return nil
	}
	for _, name := range cfgNames {
		seen[name] = true
		if err := config.ValidateName(name); err != nil {
			rows = append(rows, ServiceState{Name: name, State: "-", Error: err.Error()})
			continue
		}
		s, lerr := config.LoadService(a.Paths, name)
		if err := add(name, s, lerr); err != nil {
			return nil, err
		}
	}
	for _, name := range plistNames {
		if seen[name] {
			continue
		}
		if err := add(name, nil, nil); err != nil {
			return nil, err
		}
	}
	return rows, nil
}

func (a *App) syncReason(s *config.Service, st launchd.Status) (bool, string) {
	exists, current := a.PlistCurrent(s)
	switch {
	case !exists:
		return true, "plist missing"
	case !current:
		return true, "plist outdated"
	case st.Loaded && st.Path != "" && st.Path != a.Paths.PlistFile(s.Name):
		return true, fmt.Sprintf("loaded from %s", st.Path)
	}
	return false, ""
}

// Action kinds for Apply.
const (
	ActionRegister   = "register"
	ActionUpdate     = "update"
	ActionUnregister = "unregister"
)

// Action is one change Apply makes.
type Action struct {
	Kind string
	Name string
}

// Plan computes what Apply would do. Invalid configs are returned as
// errors and skipped.
func (a *App) Plan() ([]Action, []error, error) {
	cfgNames, err := a.Paths.ServiceNames()
	if err != nil {
		return nil, nil, err
	}
	plistNames, err := a.Paths.PlistNames()
	if err != nil {
		return nil, nil, err
	}
	var actions []Action
	var problems []error
	has := map[string]bool{}
	for _, name := range cfgNames {
		has[name] = true
		s, err := config.LoadService(a.Paths, name)
		if err != nil {
			problems = append(problems, err)
			continue
		}
		exists, current := a.PlistCurrent(s)
		switch {
		case !exists:
			actions = append(actions, Action{ActionRegister, name})
		case !current:
			actions = append(actions, Action{ActionUpdate, name})
		}
	}
	for _, name := range plistNames {
		if !has[name] {
			actions = append(actions, Action{ActionUnregister, name})
		}
	}
	return actions, problems, nil
}

// Apply aligns launchd with the config directory: new configs are
// registered, plists without a config are unregistered and outdated plists
// are regenerated (and reloaded when loaded).
func (a *App) Apply(actions []Action) error {
	for _, act := range actions {
		label := config.Label(act.Name)
		switch act.Kind {
		case ActionRegister:
			s, err := config.LoadService(a.Paths, act.Name)
			if err != nil {
				return err
			}
			if _, err := a.SyncPlist(s); err != nil {
				return err
			}
			if err := a.LD.Enable(label); err != nil {
				return err
			}
			if err := a.LD.Bootstrap(a.Paths.PlistFile(act.Name)); err != nil {
				return err
			}
		case ActionUpdate:
			s, err := config.LoadService(a.Paths, act.Name)
			if err != nil {
				return err
			}
			st, err := a.LD.Status(label)
			if err != nil {
				return err
			}
			if _, err := a.SyncPlist(s); err != nil {
				return err
			}
			if st.Loaded {
				if err := a.LD.Bootout(label); err != nil {
					return err
				}
				if err := a.LD.Bootstrap(a.Paths.PlistFile(act.Name)); err != nil {
					return err
				}
				if st.Running {
					if err := a.LD.Kickstart(label); err != nil {
						return err
					}
				}
			}
		case ActionUnregister:
			if err := a.LD.Bootout(label); err != nil {
				return err
			}
			if err := a.LD.Enable(label); err != nil {
				return err
			}
			if err := removeIfExists(a.Paths.PlistFile(act.Name)); err != nil {
				return err
			}
		}
		a.infof("%s: %s", act.Name, act.Kind)
	}
	names, err := a.Paths.ServiceNames()
	if err != nil {
		return err
	}
	if len(names) > 0 {
		return a.EnsureRotateJob()
	}
	return a.pruneRotateJob()
}

// fileSize returns the size of path, or -1 when it does not exist.
func fileSize(path string) int64 {
	info, err := os.Stat(path)
	if err != nil {
		return -1
	}
	return info.Size()
}
