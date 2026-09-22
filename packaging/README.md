# Packaging Tessera

| Directory | Builds | Tested |
|-----------|--------|--------|
| [`arch/`](arch) | `tessera-git` for Arch Linux (pacman) | Built and installed on Arch |

## Arch Linux

```bash
cd packaging/arch
makepkg -si            # build, then install with pacman
```

`makepkg` installs the build tools it needs, pacman installs the libraries
Tessera links against, and removing the package takes everything with it.

The package installs `tessera-comp`, `tessera-launcher`, `tessera-serv` and
`tessera-session` into `/usr/bin`, and
`/usr/share/wayland-sessions/tessera.desktop`, which is what makes **Tessera**
appear in a display manager's session list.

### Building from somewhere else

`TESSERA_GIT_URL` points the build at another repository — a local clone, for
instance, to package work that is not pushed yet:

```bash
TESSERA_GIT_URL=file:///home/you/Projects/tessera makepkg -si
```

It still packages a **commit**, never uncommitted work, so anything built is
something that can be checked out again.

It builds from the current `main` branch on GitHub; `pkgver()` produces
something like `0.1.0.r120.g4a4d736`, so a rebuild after new commits sorts as
an upgrade. To publish it to the AUR, generate the metadata first:

```bash
makepkg --printsrcinfo > .SRCINFO
```

### Building somewhere other than the root partition

Build output is large. To keep it off the root filesystem:

```bash
BUILDDIR=/mnt/data/tessera-build PKGDEST=/mnt/data/tessera-packages makepkg
```

Build output for this project has reached 9 GB before now.

## Debian and Fedora

Not written yet. They can be built on this machine but not *tested* on it, so
they will say so plainly when they land.
