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

type LaunchOptions struct {
	ExtraArgs  []string
	FreshStart bool
}

type Runtime interface {
	Name() string
	InstructionFile() string
	Launch(dir, workspace, socketPath, axBin, configPath string, options LaunchOptions) error
	UserCommand(dir, workspace, socketPath, axBin, configPath string, options LaunchOptions) (string, error)
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
	return RunWithOptions(name, workspace, socketPath, configPath, LaunchOptions{})
}

func RunWithArgs(name, workspace, socketPath, configPath string, extraArgs []string) error {
	return RunWithOptions(name, workspace, socketPath, configPath, LaunchOptions{ExtraArgs: extraArgs})
}

func RunWithOptions(name, workspace, socketPath, configPath string, options LaunchOptions) error {
	dir, err := CurrentDir()
	if err != nil {
		return err
	}
	return RunInDirWithOptions(name, dir, workspace, socketPath, configPath, options)
}

func RunInDir(name, dir, workspace, socketPath, configPath string) error {
	return RunInDirWithOptions(name, dir, workspace, socketPath, configPath, LaunchOptions{})
}

func RunInDirWithArgs(name, dir, workspace, socketPath, configPath string, extraArgs []string) error {
	return RunInDirWithOptions(name, dir, workspace, socketPath, configPath, LaunchOptions{ExtraArgs: extraArgs})
}

func RunInDirWithOptions(name, dir, workspace, socketPath, configPath string, options LaunchOptions) error {
	runtime, err := Get(name)
	if err != nil {
		return err
	}
	axBin, err := ResolveAxBinary()
	if err != nil {
		return err
	}
	return runtime.Launch(dir, workspace, socketPath, axBin, configPath, options)
}

func BuildUserCommand(name, dir, workspace, socketPath, axBin, configPath string) (string, error) {
	return BuildUserCommandWithOptions(name, dir, workspace, socketPath, axBin, configPath, LaunchOptions{})
}

func BuildUserCommandWithArgs(name, dir, workspace, socketPath, axBin, configPath string, extraArgs []string) (string, error) {
	return BuildUserCommandWithOptions(name, dir, workspace, socketPath, axBin, configPath, LaunchOptions{ExtraArgs: extraArgs})
}

func BuildUserCommandWithOptions(name, dir, workspace, socketPath, axBin, configPath string, options LaunchOptions) (string, error) {
	runtime, err := Get(name)
	if err != nil {
		return "", err
	}
	return runtime.UserCommand(filepath.Clean(dir), workspace, socketPath, axBin, configPath, options)
}
