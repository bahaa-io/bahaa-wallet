#!/bin/sh
set -eu

releases="https://github.com/bahaa-io/bahaa-wallet/releases/latest/download"
target="${BAHAA_INSTALL_DIR:-$HOME/.local/bin}"
key="-----BEGIN PUBLIC KEY-----
MCowBQYDK2VwAyEAyFaHNPy9LcROq4u7HSd1EBGntv5ao7eawZVJRn5X+yg=
-----END PUBLIC KEY-----"

fail() {
  echo "Bahaa Wallet couldn't be installed: $1" >&2
  exit 1
}

case "$(uname -s)" in
  Linux) system=linux ;;
  Darwin) system=macos ;;
  *) fail "there is no build for $(uname -s). On Windows, use the PowerShell command." ;;
esac

case "$(uname -m)" in
  x86_64 | amd64) processor=x86_64 ;;
  arm64 | aarch64) processor=aarch64 ;;
  *) fail "there is no build for $(uname -m) processors." ;;
esac

command -v curl >/dev/null 2>&1 || fail "curl is needed to download it."

asset="bahaa-wallet-tui-$system-$processor"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM

echo "Downloading $asset"
for name in "$asset" SHA256SUMS SHA256SUMS.sig; do
  curl --fail --silent --show-error --location --proto-redir =https \
    --output "$work/$name" "$releases/$name" || fail "$name couldn't be downloaded."
done

expected=$(awk -v name="$asset" '$2 == name || $2 == "*" name { print $1 }' "$work/SHA256SUMS")
[ -n "$expected" ] || fail "the release has no $asset."
if command -v sha256sum >/dev/null 2>&1; then
  actual=$(sha256sum "$work/$asset" | cut -d ' ' -f 1)
else
  actual=$(shasum -a 256 "$work/$asset" | cut -d ' ' -f 1)
fi
[ "$expected" = "$actual" ] || fail "the download doesn't match its checksum."

checked="checksum verified; checking the signature needs OpenSSL 3"
if command -v openssl >/dev/null 2>&1; then
  printf '%s\n' "$key" > "$work/key.pem"
  result=$(openssl pkeyutl -verify -pubin -inkey "$work/key.pem" -rawin \
    -in "$work/SHA256SUMS" -sigfile "$work/SHA256SUMS.sig" 2>&1 || true)
  case "$result" in
    *"Verified Successfully"*) checked="checksum and signature verified" ;;
    *"Verification Failure"*) fail "the release signature is not valid." ;;
  esac
fi

mkdir -p "$target"
cp "$work/$asset" "$target/bahaa-wallet.new"
chmod 755 "$target/bahaa-wallet.new"
mv -f "$target/bahaa-wallet.new" "$target/bahaa-wallet"

echo "Installed Bahaa Wallet to $target/bahaa-wallet ($checked)."
case ":$PATH:" in
  *":$target:"*) echo "Run it with: bahaa-wallet" ;;
  *)
    echo "$target is not in your PATH yet. Add this line to ~/.zshrc or ~/.bashrc:"
    echo "  export PATH=\"$target:\$PATH\""
    echo "Or run it directly: $target/bahaa-wallet"
    ;;
esac
