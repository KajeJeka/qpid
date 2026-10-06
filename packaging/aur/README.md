# AUR — `qpid`

`PKGBUILD` is a template: CI substitutes `@VERSION@` and `@SHA256@` at
release time. The checksum is computed from the tag's tarball, never
invented (linux-port.md §13).

## Publish procedure (manual)

1. **Get the rendered PKGBUILD** — download the `qpid-AUR` artifact from the
   release run; it contains this PKGBUILD with the tag's real sha256.
2. **Sanity-check on an Arch box:**
   - `updpkgsums` — the checksum must come out unchanged (if it changed, the
     tag tarball was rebuilt/moved; do not publish).
   - `makepkg --printsrcinfo > .SRCINFO`
3. **Push to the AUR** (separate repo, per §13):

   ```sh
   git clone ssh://aur@aur.archlinux.org/qpid.git
   cp PKGBUILD .SRCINFO qpid/    # from step 1-2
   cd qpid && git add PKGBUILD .SRCINFO
   git commit -m "Update to vX.Y.Z" && git push
   ```

Push stays **manual** until the user has an AUR account and a CI SSH secret
exists — do not automate the push.
