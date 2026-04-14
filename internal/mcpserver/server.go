package mcpserver

import (
	"fmt"
	"log"
	"os"
	"strings"

	"github.com/ashon/ax/internal/daemon"
	"github.com/mark3labs/mcp-go/server"
)

// Run starts the MCP server with stdio transport, connecting to the daemon.
func Run(workspace, socketPath, configPath string) error {
	if socketPath == "" {
		socketPath = daemon.DefaultSocketPath
	}

	logger := log.New(os.Stderr, fmt.Sprintf("[ax-mcp:%s] ", workspace), log.LstdFlags)

	// Connect to daemon
	client := NewDaemonClient(socketPath, workspace)
	if dir, err := os.Getwd(); err == nil {
		description := ""
		if _, cfg, err := loadToolConfig(configPath); err == nil {
			if ws, ok := cfg.Workspaces[workspace]; ok {
				description = strings.TrimSpace(ws.Description)
			}
		}
		client.SetRegistrationInfo(dir, description)
	}
	if err := client.Connect(); err != nil {
		return fmt.Errorf("connect to daemon: %w", err)
	}
	defer client.Close()

	logger.Printf("connected to daemon, registered as %q", workspace)

	// Create MCP server
	srv := server.NewMCPServer(
		"ax",
		"0.1.0",
		server.WithToolCapabilities(true),
		server.WithInstructions(fmt.Sprintf(
			"You are the %q workspace agent in an ax multi-agent environment. "+
				"Use these tools to coordinate with other workspace agents. "+
				"Call list_agents to inspect configured agents from the active ax config, call list_workspaces to see who is currently active, and read_messages periodically "+
				"to check for incoming messages from other agents.",
			workspace,
		)),
	)

	// Register tools
	registerTools(srv, client, configPath)

	logger.Println("MCP server ready, serving on stdio")

	// Run with stdio transport
	if err := server.ServeStdio(srv); err != nil {
		return fmt.Errorf("serve stdio: %w", err)
	}

	return nil
}
