# Command-line conventions
Type: feature

## Purpose

The arguments, multi-service handling, and exit status shared by every hum command. Owned by the CLI module.

## Requirements

### Requirement: Multiple names and --all
`start`, `stop`, `restart`, `rm`, and `logs` MUST accept several service names or `--all` (every service with a config file), but not both. When one service fails in `start`, `stop`, `restart`, `rm`, or `logs --rotate`, hum MUST report it, carry on with the rest, and finally exit with a non-zero status.

### Requirement: Skip confirmation
The global `-y` flag MUST skip every confirmation prompt and count as agreeing.

### Requirement: Completion scripts
`hum completion fish|zsh|bash` MUST print the completion script for that shell, and completion MUST offer the configured service names.

### Requirement: Exit status on failure
When any command fails, it MUST print the reason to standard error and exit with a non-zero status.
