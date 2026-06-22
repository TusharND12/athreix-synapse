#!/usr/bin/env node
// Thin launcher: exec the native `synapse` binary fetched by install.js.
const path = require("path");
const { spawnSync } = require("child_process");

const bin = path.join(
  __dirname,
  process.platform === "win32" ? "synapse.exe" : "synapse-bin",
);

const r = spawnSync(bin, process.argv.slice(2), { stdio: "inherit" });
if (r.error) {
  console.error(
    "[synapse] binary not found. Reinstall, or build from source:\n" +
      "  cargo install --git https://github.com/TusharND12/athreix-synapse synapse-cli",
  );
  process.exit(1);
}
process.exit(r.status === null ? 1 : r.status);
