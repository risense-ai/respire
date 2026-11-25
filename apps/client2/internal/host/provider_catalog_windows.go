package host

import "respire-client2/internal/i18n"

import (
	"archive/zip"
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"runtime"
)

const (
	winMLVersion     = "2.4.89"
	winMLCatalogName = "Microsoft.Windows.AI.MachineLearning.dll"
	winMLPackageURL  = "https://api.nuget.org/v3-flatcontainer/microsoft.windows.ai.machinelearning/" + winMLVersion + "/microsoft.windows.ai.machinelearning." + winMLVersion + ".nupkg"
	winMLPackageHash = "5c68ecfb947223267abf159a023f5192ad42725e4e9cc995e7c1470dd54dff63"
)

type winMLCatalog struct {
	arch, digest string
	size         uint64
	dll, license []byte
}

func (m *Manager) downloadWinMLCatalog(ctx context.Context) (winMLCatalog, error) {
	var catalog winMLCatalog
	// These pins match the CLI catalog from the official, pinned NuGet package.
	switch runtime.GOARCH {
	case "amd64":
		catalog = winMLCatalog{arch: "x64", size: 971504, digest: "b401a47552a745755d65100828faefcf3df3895c759f96ac625ddf990711c8dd"}
	case "arm64":
		catalog = winMLCatalog{arch: "arm64", size: 1091328, digest: "457295ca2aec34db4aec7064cb401520bc85f29fd18f7e7bb9e25ddb849bedb1"}
	default:
		return catalog, errors.New(i18n.Messagee6ef22c44ab93a8b)
	}
	stage, err := os.MkdirTemp(m.Root, ".winml-*")
	if err != nil {
		return catalog, err
	}
	defer os.RemoveAll(stage)
	archive := filepath.Join(stage, "winml.nupkg")
	client := *m.HTTPClient
	client.CheckRedirect = func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }
	if err := m.downloadOnce(ctx, &client, winMLPackageURL, archive, winMLPackageHash, 128<<20, false); err != nil {
		return catalog, fmt.Errorf(i18n.Message1705b807d1f7ad35, err)
	}
	err = catalog.readArchive(archive)
	return catalog, err
}

func (c *winMLCatalog) readArchive(path string) error {
	z, err := zip.OpenReader(path)
	if err != nil {
		return err
	}
	defer z.Close()
	member := "runtimes/win-" + c.arch + "/native/" + winMLCatalogName
	c.dll, err = readWinMLMember(z.File, member, c.size)
	if err != nil {
		return err
	}
	if uint64(len(c.dll)) != c.size || controlDigest(c.dll) != c.digest {
		return errors.New(i18n.Message4063a9b528794523)
	}
	c.license, err = readWinMLMember(z.File, "license.txt", 1<<20)
	return err
}

func readWinMLMember(files []*zip.File, name string, limit uint64) ([]byte, error) {
	var member *zip.File
	for _, file := range files {
		if file.Name != name {
			continue
		}
		if member != nil || !file.Mode().IsRegular() || file.UncompressedSize64 == 0 || file.UncompressedSize64 > limit {
			return nil, fmt.Errorf(i18n.Messagef2ca8ece89d7413a, name)
		}
		member = file
	}
	if member == nil {
		return nil, fmt.Errorf(i18n.Message31ab4d4e76e0870d, name)
	}
	reader, err := member.Open()
	if err != nil {
		return nil, err
	}
	defer reader.Close()
	data, err := io.ReadAll(io.LimitReader(reader, int64(limit)+1))
	if err != nil {
		return nil, err
	}
	if uint64(len(data)) != member.UncompressedSize64 {
		return nil, fmt.Errorf(i18n.Messageaacd583f226099cb, name)
	}
	return data, nil
}

func (c winMLCatalog) publish(root string) error {
	dir, err := deviceProfilePath(filepath.Join(root, "engines", "winml-"+winMLVersion))
	if err != nil {
		return err
	}
	if err := os.MkdirAll(dir, 0700); err != nil {
		return err
	}
	if err := publishWinMLFile(filepath.Join(dir, "license.txt"), c.license); err != nil {
		return err
	}
	return publishWinMLFile(filepath.Join(dir, winMLCatalogName), c.dll)
}

func publishWinMLFile(path string, data []byte) error {
	old, err := readCredentialConfig(path, 1<<20+128<<10)
	if err == nil && bytes.Equal(old, data) {
		return nil
	}
	if err != nil && !errors.Is(err, os.ErrNotExist) {
		return err
	}
	temp, err := os.CreateTemp(filepath.Dir(path), ".winml-*")
	if err != nil {
		return err
	}
	defer os.Remove(temp.Name())
	_, writeErr := temp.Write(data)
	if err := errors.Join(writeErr, temp.Sync(), temp.Close()); err != nil {
		return err
	}
	if old == nil {
		return publishFreshRuntimeFile(temp.Name(), path)
	}
	return publishSessionFile(temp.Name(), path)
}
