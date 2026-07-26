// CI/CD for fastmail-cli.
//
// The release binary is the unit of caching: each linux platform is compiled
// once and that same file is reused by both the release tarball and the
// container image, so nothing is ever built from source twice.
//
// Only one linux platform is native to the engine; the other is cross-compiled
// with a gcc cross toolchain rather than emulated, because kreuzberg's
// bundled-pdfium fetches a prebuilt library per target instead of building C++
// from source, which leaves nothing that actually needs to run as the foreign
// architecture. Emulating a full Rust build would cost far more.
//
// macOS binaries are not built here, for a dependency reason rather than a
// Dagger one: aws-lc-sys, pulled in by rustls, does not cross-compile to darwin
// from Linux. CI builds those natively on a macOS runner and passes them in via
// the darwinAssets argument.

package main

import (
	"context"
	"errors"
	"fmt"
	"path"
	"regexp"
	"strings"

	"dagger/fastmail-cli/internal/dagger"
)

const (
	rustImage    = "rust:1-bookworm"
	runtimeImage = "debian:bookworm-slim"
	tarImage     = "alpine:3.22"
)

type FastmailCli struct {
	// +private
	Source *dagger.Directory
}

func New(
	// The fastmail-cli source tree
	// +defaultPath="/"
	// +ignore=["/target", "/.git", "/.github", "/.dagger", "/CHANGELOG.md"]
	source *dagger.Directory,
) *FastmailCli {
	return &FastmailCli{Source: source}
}

// A linux platform and everything needed to build for it.
type target struct {
	platform dagger.Platform
	// Rust target triple, e.g. x86_64-unknown-linux-gnu
	triple string
	// Cross toolchain binary prefix, e.g. x86_64-linux-gnu-gcc
	toolPrefix string
	// Debian cross-compiler package suffix — not always the triple: amd64 is
	// gcc-x86-64-linux-gnu, with dashes where the triple has an underscore.
	debSuffix string
	// Suffix used in release artefact names, matching the existing releases.
	artifact string
}

var targets = []target{
	{"linux/amd64", "x86_64-unknown-linux-gnu", "x86_64-linux-gnu", "x86-64-linux-gnu", "linux-x86_64"},
	{"linux/arm64", "aarch64-unknown-linux-gnu", "aarch64-linux-gnu", "aarch64-linux-gnu", "linux-aarch64"},
}

func targetFor(platform dagger.Platform) (target, error) {
	for _, t := range targets {
		if t.platform == platform {
			return t, nil
		}
	}
	return target{}, fmt.Errorf("unsupported platform %q: want linux/amd64 or linux/arm64", platform)
}

// A rust toolchain with the project source and warm cargo caches.
//
// cacheKey partitions the target directory. The three checks deliberately share
// one: compiling the dependency tree dominates this build, cargo only applies
// clippy to workspace members, so dependency artefacts are reused across them.
// Cargo's own lock serialises whatever overlaps, which is cheaper than
// compiling the tree once per check.
func (m *FastmailCli) base(cacheKey string) *dagger.Container {
	return dag.Container().
		From(rustImage).
		WithExec([]string{"apt-get", "update"}).
		WithExec([]string{"apt-get", "install", "-y", "--no-install-recommends",
			"clang", "cmake", "pkg-config"}).
		// The official rust image installs the toolchain with the minimal
		// profile, which is cargo, rustc and rust-std only — neither clippy nor
		// rustfmt is present. The layer is shared with every other use of this
		// container, so adding them here costs nothing.
		WithExec([]string{"rustup", "component", "add", "clippy", "rustfmt"}).
		// Cargo serialises concurrent downloads through a lock file it keeps at
		// the root of CARGO_HOME. Mounting only registry/ and git/ left that
		// lock on each container's own filesystem, so containers sharing the
		// registry could not see each other's and raced to unpack the same
		// crate — which corrupts it. Putting all of CARGO_HOME on the volume
		// keeps the lock beside the data it guards. The toolchain itself lives
		// under RUSTUP_HOME and is unaffected.
		WithEnvVariable("CARGO_HOME", "/cargo").
		WithMountedCache("/cargo", dag.CacheVolume("cargo-home")).
		WithWorkdir("/src").
		WithDirectory("/src", m.Source).
		WithMountedCache("/src/target", dag.CacheVolume("cargo-target-"+cacheKey))
}

// Rustfmt is clean.
// +check
func (m *FastmailCli) Fmt(ctx context.Context) error {
	_, err := m.base("check").WithExec([]string{"cargo", "fmt", "--", "--check"}).Sync(ctx)
	return err
}

// Clippy raises no warnings.
// +check
func (m *FastmailCli) Clippy(ctx context.Context) error {
	_, err := m.base("check").
		WithExec([]string{"cargo", "clippy", "--locked", "--", "-D", "warnings"}).
		Sync(ctx)
	return err
}

// The test suite passes.
//
// This also covers what a separate `cargo build` step used to: cargo compiles
// the binary target to run the tests, so building it again proved nothing.
// +check
func (m *FastmailCli) Test(ctx context.Context) error {
	_, err := m.base("check").WithExec([]string{"cargo", "test", "--locked"}).Sync(ctx)
	return err
}

// Build the release binary for a linux platform.
func (m *FastmailCli) Binary(
	ctx context.Context,
	// +default="linux/amd64"
	platform dagger.Platform,
) (*dagger.File, error) {
	t, err := targetFor(platform)
	if err != nil {
		return nil, err
	}

	native, err := dag.DefaultPlatform(ctx)
	if err != nil {
		return nil, err
	}

	build := m.base(t.triple)
	if platform != native {
		build = crossToolchain(build, t)
	}

	// The target directory is a cache mount, and mounts are not part of the
	// container's filesystem snapshot — the binary has to be copied off it
	// within the same exec or it is not there to read afterwards.
	return build.
		WithExec([]string{"sh", "-c",
			"cargo build --release --locked --target " + t.triple +
				" && install -D target/" + t.triple + "/release/fastmail /out/fastmail"}).
		File("/out/fastmail"), nil
}

// Point cargo and cc-rs at a gcc cross toolchain for the given target.
func crossToolchain(c *dagger.Container, t target) *dagger.Container {
	env := strings.ReplaceAll(strings.ToUpper(t.triple), "-", "_")
	underscored := strings.ReplaceAll(t.triple, "-", "_")

	return c.
		WithExec([]string{"apt-get", "install", "-y", "--no-install-recommends",
			"gcc-" + t.debSuffix, "g++-" + t.debSuffix}).
		WithExec([]string{"rustup", "target", "add", t.triple}).
		WithEnvVariable("CARGO_TARGET_"+env+"_LINKER", t.toolPrefix+"-gcc").
		WithEnvVariable("CC_"+underscored, t.toolPrefix+"-gcc").
		WithEnvVariable("CXX_"+underscored, t.toolPrefix+"-g++").
		WithEnvVariable("AR_"+underscored, t.toolPrefix+"-ar").
		// bindgen resolves headers against the host by default.
		WithEnvVariable("BINDGEN_EXTRA_CLANG_ARGS_"+underscored,
			"--sysroot=/usr/"+t.toolPrefix).
		WithEnvVariable("PKG_CONFIG_ALLOW_CROSS", "1")
}

// Package the release binary as the tarball published to GitHub releases.
func (m *FastmailCli) Tarball(
	ctx context.Context,
	// +default="linux/amd64"
	platform dagger.Platform,
) (*dagger.File, error) {
	t, err := targetFor(platform)
	if err != nil {
		return nil, err
	}
	bin, err := m.Binary(ctx, platform)
	if err != nil {
		return nil, err
	}

	name := "fastmail-cli-" + t.artifact + ".tar.gz"
	return dag.Container().
		From(tarImage).
		WithWorkdir("/out").
		WithFile("/out/fastmail", bin).
		WithExec([]string{"tar", "-czf", name, "fastmail"}).
		File("/out/" + name), nil
}

// The runtime image: the prebuilt binary on debian-slim.
//
// Default command runs the MCP server over HTTP — this is the image the
// mcp-gateway proxies to as a backend (token per request via X-Fastmail-Token).
func (m *FastmailCli) Image(
	ctx context.Context,
	// +default="linux/amd64"
	platform dagger.Platform,
) (*dagger.Container, error) {
	bin, err := m.Binary(ctx, platform)
	if err != nil {
		return nil, err
	}

	// ca-certificates is architecture-independent data, so it is unpacked once
	// on the native platform and copied into every variant rather than running
	// dpkg under emulation. Both paths matter: /etc/ssl/certs holds the bundle
	// plus hash symlinks that point into /usr/share/ca-certificates.
	certs := caCertificates()

	return dag.Container(dagger.ContainerOpts{Platform: platform}).
		From(runtimeImage).
		WithDirectory("/usr/share/ca-certificates", certs.Directory("/usr/share/ca-certificates")).
		WithDirectory("/etc/ssl/certs", certs.Directory("/etc/ssl/certs")).
		WithFile("/usr/local/bin/fastmail", bin).
		WithExposedPort(8080).
		WithEntrypoint([]string{"fastmail"}).
		WithDefaultArgs([]string{"mcp", "--http", "0.0.0.0:8080"}), nil
}

func caCertificates() *dagger.Container {
	return dag.Container().
		From(runtimeImage).
		WithExec([]string{"apt-get", "update"}).
		WithExec([]string{"apt-get", "install", "-y", "--no-install-recommends", "ca-certificates"})
}

// Publish one multi-platform manifest per tag.
//
// Dagger pushes the manifest list itself, so there are no per-architecture
// staging tags to assemble afterwards.
func (m *FastmailCli) PublishImage(
	ctx context.Context,
	// Full image address without a tag, e.g. ghcr.io/radiosilence/fastmail-cli
	image string,
	tags []string,
	username string,
	password *dagger.Secret,
) ([]string, error) {
	variants := make([]*dagger.Container, 0, len(targets))
	for _, t := range targets {
		v, err := m.Image(ctx, t.platform)
		if err != nil {
			return nil, err
		}
		variants = append(variants, v)
	}

	published := make([]string, 0, len(tags))
	for _, tag := range tags {
		ref, err := variants[0].
			WithRegistryAuth(image, username, password).
			Publish(ctx, image+":"+tag, dagger.ContainerPublishOpts{PlatformVariants: variants})
		if err != nil {
			return nil, fmt.Errorf("publish %s: %w", tag, err)
		}
		published = append(published, ref)
	}
	return published, nil
}

// The version declared in Cargo.toml.
func (m *FastmailCli) Version(ctx context.Context) (string, error) {
	manifest, err := m.Source.File("Cargo.toml").Contents(ctx)
	if err != nil {
		return "", err
	}
	match := regexp.MustCompile(`(?m)^version\s*=\s*"([^"]+)"`).FindStringSubmatch(manifest)
	if match == nil {
		return "", errors.New("no version found in Cargo.toml")
	}
	return match[1], nil
}

// Image tags for a build of main. The semver and latest tags are only moved
// when the version in Cargo.toml has not been released yet.
func imageTags(version, commit string, isNewVersion bool) []string {
	if len(commit) > 7 {
		commit = commit[:7]
	}
	tags := []string{"main", "sha-" + commit}
	if !isNewVersion {
		return tags
	}
	parts := strings.Split(version, ".")
	return append(tags,
		"v"+version,
		"v"+strings.Join(parts[:2], "."),
		"v"+parts[0],
		"latest",
	)
}

// Whether this version has already been released.
//
// A missing release and a failure to ask both surface as an error from gh, so
// this treats any error as "not released". That errs toward attempting the
// release, which fails loudly, rather than silently skipping one.
func alreadyReleased(ctx context.Context, repo, tag string, token *dagger.Secret) bool {
	_, err := dag.Gh(dagger.GhOpts{Token: token, Repo: repo}).
		Exec([]string{"release", "view", tag}).
		Sync(ctx)
	return err == nil
}

// Release assets that Dagger cannot build and so has to be handed. Named
// exactly, because a release that is quietly missing a platform is worse than a
// failed build: it looks complete to anyone downloading it.
var darwinArtifacts = []string{
	"fastmail-cli-darwin-x86_64.tar.gz",
	"fastmail-cli-darwin-aarch64.tar.gz",
}

// Resolve the macOS tarballs, failing unless every expected one is present.
func releaseAssets(ctx context.Context, dir *dagger.Directory) ([]*dagger.File, error) {
	if dir == nil {
		return nil, fmt.Errorf("no darwin assets given, expected %s", strings.Join(darwinArtifacts, ", "))
	}

	// The upload/download-artifact round trip decides whether these land at the
	// root or under a per-artifact directory, so match on name either way.
	found, err := dir.Glob(ctx, "**/*.tar.gz")
	if err != nil {
		return nil, err
	}
	byName := make(map[string]string, len(found))
	for _, p := range found {
		byName[path.Base(p)] = p
	}

	var assets []*dagger.File
	var missing []string
	for _, want := range darwinArtifacts {
		p, ok := byName[want]
		if !ok {
			missing = append(missing, want)
			continue
		}
		assets = append(assets, dir.File(p))
	}
	if len(missing) > 0 {
		return nil, fmt.Errorf("missing release assets: %s (found: %s)",
			strings.Join(missing, ", "), strings.Join(found, ", "))
	}
	return assets, nil
}

// Build, publish images, and cut a GitHub release for a push to main.
//
// Images are pushed before the release is created: this project is consumed as
// a container, so a version tag with no image behind it is worse than no tag at
// all — and an auto-updater polling releases will happily pin one.
func (m *FastmailCli) Deliver(
	ctx context.Context,
	// owner/name of the GitHub repository
	repo string,
	// Full image address without a tag, e.g. ghcr.io/radiosilence/fastmail-cli
	image string,
	// Commit being built: the sha- image tag, and what the release tag points at
	commit string,
	registryUsername string,
	registryPassword *dagger.Secret,
	githubToken *dagger.Secret,
	// Release assets built outside Dagger — the macOS tarballs. Required when
	// this version has not been released yet.
	// +optional
	darwinAssets *dagger.Directory,
) (string, error) {
	version, err := m.Version(ctx)
	if err != nil {
		return "", err
	}
	tag := "v" + version
	isNew := !alreadyReleased(ctx, repo, tag, githubToken)

	// Assets are resolved before anything is pushed. Publishing first and
	// discovering a missing tarball afterwards would leave the semver and latest
	// image tags moved with no release behind them.
	var assets []*dagger.File
	if isNew {
		linux, err := m.Tarball(ctx, "linux/amd64")
		if err != nil {
			return "", err
		}
		darwin, err := releaseAssets(ctx, darwinAssets)
		if err != nil {
			return "", err
		}
		assets = append([]*dagger.File{linux}, darwin...)
	}

	refs, err := m.PublishImage(ctx, image, imageTags(version, commit, isNew), registryUsername, registryPassword)
	if err != nil {
		return "", err
	}
	summary := fmt.Sprintf("published %d image tags: %s", len(refs), strings.Join(refs, ", "))

	if !isNew {
		return summary + "\nno release: " + tag + " is already released", nil
	}

	err = dag.Gh(dagger.GhOpts{Token: githubToken, Repo: repo}).
		Release().
		Create(ctx, tag, tag, dagger.GhReleaseCreateOpts{
			Files:         assets,
			GenerateNotes: true,
			Target:        commit,
		})
	if err != nil {
		return "", fmt.Errorf("create release %s: %w", tag, err)
	}
	return fmt.Sprintf("%s\nreleased %s with %d assets", summary, tag, len(assets)), nil
}
