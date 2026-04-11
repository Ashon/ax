package agent

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

const (
	RuntimeClaude = "claude"
	RuntimeCodex  = "codex"
)

var supportedRuntimeNames = []string{
	RuntimeClaude,
	RuntimeCodex,
}

type Runtime interface {
	Name() string
	InstructionFile() string
	Launch(dir, workspace, socketPath, axBin, configPath string) error
	UserCommand(dir, workspace, socketPath, axBin, configPath string) (string, error)
}

func NormalizeRuntime(name string) string {
	switch strings.ToLower(strings.TrimSpace(name)) {
	case "", RuntimeClaude:
		return RuntimeClaude
	case RuntimeCodex:
		return RuntimeCodex
	default:
		return strings.ToLower(strings.TrimSpace(name))
	}
}

func Get(name string) (Runtime, error) {
	switch NormalizeRuntime(name) {
	case RuntimeClaude:
		return claudeRuntime{}, nil
	case RuntimeCodex:
		return codexRuntime{}, nil
	default:
		return nil, fmt.Errorf("unsupported runtime %q", name)
	}
}

func SupportedNames() []string {
	names := make([]string, len(supportedRuntimeNames))
	copy(names, supportedRuntimeNames)
	return names
}

func CurrentDir() (string, error) {
	dir, err := os.Getwd()
	if err != nil {
		return "", fmt.Errorf("resolve cwd: %w", err)
	}
	return dir, nil
}

func ResolveAxBinary() (string, error) {
	path, err := os.Executable()
	if err != nil {
		return "", fmt.Errorf("resolve ax binary: %w", err)
	}
	return path, nil
}

func InstructionFile(name string) (string, error) {
	runtime, err := Get(name)
	if err != nil {
		return "", err
	}
	return runtime.InstructionFile(), nil
}

func Run(name, workspace, socketPath, configPath string) error {
	dir, err := CurrentDir()
	if err != nil {
		return err
	}
	return RunInDir(name, dir, workspace, socketPath, configPath)
}

func RunInDir(name, dir, workspace, socketPath, configPath string) error {
	runtime, err := Get(name)
	if err != nil {
		return err
	}
	axBin, err := ResolveAxBinary()
	if err != nil {
		return err
	}
	return runtime.Launch(dir, workspace, socketPath, axBin, configPath)
}

func BuildUserCommand(name, dir, workspace, socketPath, axBin, configPath string) (string, error) {
	runtime, err := Get(name)
	if err != nil {
		return "", err
	}
	return runtime.UserCommand(filepath.Clean(dir), workspace, socketPath, axBin, configPath)
}
