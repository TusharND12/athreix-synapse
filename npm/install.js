// Postinstall: download the prebuilt `synapse` binary for this platform from the
// matching GitHub Release. Fails soft (you can always `cargo install` from source).
const fs = require("fs");
const path = require("path");
const https = require("https");
const { version } = require("./package.json");

const REPO = "TusharND12/athreix-synapse";

function assetName() {
  const os =
    { win32: "windows", darwin: "macos", linux: "linux" }[process.platform] || null;
  const arch = { x64: "x64", arm64: "arm64" }[process.arch] || null;
  if (!os || !arch) return null;
  const ext = process.platform === "win32" ? ".exe" : "";
  return `synapse-${os}-${arch}${ext}`;
}

function download(url, dest, cb, redirects = 0) {
  if (redirects > 5) return cb(new Error("too many redirects"));
  https
    .get(url, { headers: { "User-Agent": "athreix-synapse-installer" } }, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        res.resume();
        return download(res.headers.location, dest, cb, redirects + 1);
      }
      if (res.statusCode !== 200) {
        res.resume();
        return cb(new Error("HTTP " + res.statusCode));
      }
      const file = fs.createWriteStream(dest);
      res.pipe(file);
      file.on("finish", () => file.close(() => cb(null)));
      file.on("error", cb);
    })
    .on("error", cb);
}

const asset = assetName();
if (!asset) {
  console.warn(`[synapse] no prebuilt binary for ${process.platform}/${process.arch}.`);
  console.warn(`[synapse] build from source: cargo install --git https://github.com/${REPO} synapse-cli`);
  process.exit(0);
}

const binDir = path.join(__dirname, "bin");
fs.mkdirSync(binDir, { recursive: true });
const out = path.join(binDir, process.platform === "win32" ? "synapse.exe" : "synapse-bin");
const url = `https://github.com/${REPO}/releases/download/v${version}/${asset}`;

console.log(`[synapse] downloading ${asset} …`);
download(url, out, (err) => {
  if (err) {
    console.warn(`[synapse] download failed (${err.message}).`);
    console.warn(`[synapse] build from source: cargo install --git https://github.com/${REPO} synapse-cli`);
    process.exit(0); // soft-fail so `npm install` doesn't break
  }
  if (process.platform !== "win32") {
    try {
      fs.chmodSync(out, 0o755);
    } catch {}
  }
  console.log("[synapse] installed. Run `synapse` in any project.");
});
