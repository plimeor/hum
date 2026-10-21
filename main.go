// Command hum registers long-running commands as launchd services and
// manages them through one directory of TOML files.
package main

import (
	"os"

	"github.com/plimeor/hum/internal/cli"
)

func main() {
	os.Exit(cli.Execute())
}
