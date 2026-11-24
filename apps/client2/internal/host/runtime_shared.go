package host

import "respire-client2/internal/i18n"

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"time"
)

type releaseProof struct {
	Hash string `json:"metadata_sha256"`
	JSON string `json:"metadata_json"`
}

type sharedRuntimeReceipt struct {
	Version string        `json:"version"`
	SHA256  string        `json:"sha256"`
	Release *releaseProof `json:"release,omitempty"`
}

type releaseFile struct {
	Path string `json:"path"`
	Size int64  `json:"size"`
	Hash string `json:"sha256"`
}

func (m *Manager) runtimeManifest() string { return filepath.Join(m.DataRoot, "bin", "runtime.json") }
func (m *Manager) runtimeReceiptPath(v string) string {
	return filepath.Join(m.DataRoot, "bin", "runtime", ".receipts", v+".json")
}

func (m *Manager) saveRuntimeReceipt(receipt sharedRuntimeReceipt) error {
	path := m.runtimeReceiptPath(receipt.Version)
	if err := os.MkdirAll(filepath.Dir(path), 0700); err != nil {
		return err
	}
	return writeJSON(path, receipt)
}

func runtimeReceipt(meta RuntimeMetadata) (sharedRuntimeReceipt, error) {
	sum := sha256.Sum256([]byte(meta.ProofJSON))
	receipt := sharedRuntimeReceipt{Version: meta.RuntimeVersion, SHA256: meta.BinaryHash,
		Release: &releaseProof{Hash: hex.EncodeToString(sum[:]), JSON: meta.ProofJSON}}
	_, err := receiptFiles(receipt)
	return receipt, err
}

func receiptFiles(receipt sharedRuntimeReceipt) ([]releaseFile, error) {
	if !validVersion(receipt.Version) || !validHash(receipt.SHA256) || receipt.Release == nil || len(receipt.Release.JSON) > 2<<20 {
		return nil, errors.New(i18n.Message1ba94915d5699edb)
	}
	sum := sha256.Sum256([]byte(receipt.Release.JSON))
	if !strings.EqualFold(hex.EncodeToString(sum[:]), receipt.Release.Hash) {
		return nil, errors.New(i18n.Messagef8c3838a5e2da01e)
	}
	meta, err := NormalizeRuntimeMetadata([]byte(receipt.Release.JSON))
	if err != nil {
		return nil, err
	}
	target, err := runtimeTarget()
	if err != nil {
		return nil, err
	}
	if err = CheckRuntimeMetadata(meta, "v"+receipt.Version, target); err != nil {
		return nil, err
	}
	if meta.Schema != 2 || !strings.EqualFold(meta.BinaryHash, receipt.SHA256) {
		return nil, errors.New(i18n.Message77082168ef2fa8e3)
	}
	var inventory struct {
		Files    []releaseFile `json:"files"`
		Commit   string        `json:"source_commit"`
		Protocol struct {
			Minimum int `json:"minimum"`
			Maximum int `json:"maximum"`
		} `json:"protocol_range"`
	}
	if err = json.Unmarshal([]byte(receipt.Release.JSON), &inventory); err != nil {
		return nil, err
	}
	if len(inventory.Commit) != 40 || inventory.Protocol.Minimum < 1 || inventory.Protocol.Maximum < inventory.Protocol.Minimum || len(inventory.Files) == 0 || len(inventory.Files) > 4096 {
		return nil, errors.New(i18n.Message89a5f726afd59f27)
	}
	if _, err = hex.DecodeString(inventory.Commit); err != nil {
		return nil, errors.New(i18n.Message98b33b988def9d50)
	}
	seen := map[string]bool{}
	var total int64
	for i := range inventory.Files {
		file := &inventory.Files[i]
		if !filepath.IsLocal(filepath.FromSlash(file.Path)) || strings.ContainsAny(file.Path, "\\:") || !validHash(file.Hash) || file.Size <= 0 || file.Size > 256<<20 || seen[strings.ToLower(file.Path)] {
			return nil, errors.New(i18n.Message05c2a6f5e7136e8a)
		}
		seen[strings.ToLower(file.Path)] = true
		total += file.Size
		if file.Path == meta.Binary {
			if !strings.EqualFold(file.Hash, receipt.SHA256) {
				return nil, errors.New(i18n.Messagea78701afb0446dcc)
			}
			file.Path = runtimeBinary()
		}
	}
	if total > 512<<20 || !seen[strings.ToLower(meta.Binary)] || !seen["core-runtime.json"] {
		return nil, errors.New(i18n.Messageb7a8073b3fb1dac4)
	}
	return inventory.Files, nil
}

func regularRuntimeFile(root, name string) (string, error) {
	path := root
	for _, part := range append([]string{"."}, strings.Split(filepath.FromSlash(name), string(filepath.Separator))...) {
		path = filepath.Join(path, part)
		info, err := os.Lstat(path)
		if err != nil {
			return "", err
		}
		if info.Mode()&os.ModeSymlink != 0 {
			return "", errors.New(i18n.Message3d96e71aed25ed3a)
		}
	}
	info, err := os.Stat(path)
	if err != nil {
		return "", err
	}
	if !info.Mode().IsRegular() {
		return "", errors.New(i18n.Messagec0d49d1886598c8d)
	}
	return path, nil
}

func copyReleaseTree(source, dest string, receipt sharedRuntimeReceipt) error {
	files, err := receiptFiles(receipt)
	if err != nil {
		return err
	}
	target, err := runtimeTarget()
	if err != nil {
		return err
	}
	for _, file := range files {
		name := file.Path
		if name == runtimeBinary() {
			if _, err := os.Stat(filepath.Join(source, name)); errors.Is(err, os.ErrNotExist) {
				name = "rsrs-runtime-" + target + binaryExtension()
			}
		}
		if strings.HasPrefix(name, "core-notices/") {
			continue
		}
		input, err := regularRuntimeFile(source, name)
		if err != nil {
			return err
		}
		info, err := os.Stat(input)
		if err != nil {
			return err
		}
		hash, err := fileHash(input)
		if err != nil {
			return err
		}
		if info.Size() != file.Size || !strings.EqualFold(hash, file.Hash) {
			return fmt.Errorf(i18n.Message4d1d5553fb8722c1, file.Path)
		}
		output := filepath.Join(dest, filepath.FromSlash(file.Path))
		if err = os.MkdirAll(filepath.Dir(output), 0700); err != nil {
			return err
		}
		in, err := os.Open(input)
		if err != nil {
			return err
		}
		out, err := os.OpenFile(output, os.O_WRONLY|os.O_CREATE|os.O_EXCL, info.Mode().Perm())
		if err != nil {
			in.Close()
			return err
		}
		_, copyErr := io.Copy(out, in)
		err = errors.Join(copyErr, out.Sync(), out.Close(), in.Close())
		if err != nil {
			return err
		}
	}
	return verifyReleaseTree(dest, receipt)
}

func verifyReleaseTree(dir string, receipt sharedRuntimeReceipt) error {
	files, err := receiptFiles(receipt)
	if err != nil {
		return err
	}
	expected := map[string]bool{}
	for _, file := range files {
		if strings.HasPrefix(file.Path, "core-notices/") {
			if _, err := os.Lstat(filepath.Join(dir, filepath.FromSlash(file.Path))); errors.Is(err, os.ErrNotExist) {
				continue
			} else if err != nil {
				return err
			}
		}
		path, err := regularRuntimeFile(dir, file.Path)
		if err != nil {
			return err
		}
		info, err := os.Stat(path)
		if err != nil {
			return err
		}
		hash, err := fileHash(path)
		if err != nil {
			return err
		}
		if info.Size() != file.Size || !strings.EqualFold(hash, file.Hash) {
			return fmt.Errorf(i18n.Message4d1d5553fb8722c1, file.Path)
		}
		expected[path] = true
	}
	if err = filepath.WalkDir(dir, func(path string, entry os.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if !entry.IsDir() && !expected[path] {
			return fmt.Errorf(i18n.Message2532ec53c52d0ed0, path)
		}
		return nil
	}); err != nil {
		return err
	}
	return verifyRuntime(dir, receipt.SHA256)
}

func (m *Manager) selectRuntime(info InstalledRuntime) error {
	b, err := os.ReadFile(m.runtimeReceiptPath(info.Version))
	if err != nil {
		return err
	}
	var receipt sharedRuntimeReceipt
	if err = json.Unmarshal(b, &receipt); err != nil {
		return err
	}
	if receipt.Version != info.Version || receipt.SHA256 != info.SHA256 {
		return errors.New(i18n.Message1d7394beb1297dcc)
	}
	if err = verifyReleaseTree(m.runtimeDir(info.Version), receipt); err != nil {
		return err
	}
	return writeJSON(m.runtimeManifest(), receipt)
}

func (m *Manager) verifyInstalled(info InstalledRuntime) error {
	b, err := os.ReadFile(m.runtimeReceiptPath(info.Version))
	if err != nil {
		return err
	}
	var receipt sharedRuntimeReceipt
	if err = json.Unmarshal(b, &receipt); err != nil {
		return err
	}
	if receipt.Version != info.Version || receipt.SHA256 != info.SHA256 {
		return errors.New(i18n.Message1d7394beb1297dcc)
	}
	return verifyReleaseTree(m.runtimeDir(info.Version), receipt)
}

func (m *Manager) lockRuntime(ctx context.Context) (*os.File, error) {
	root := filepath.Join(m.DataRoot, "bin", "runtime")
	if err := os.MkdirAll(root, 0700); err != nil {
		return nil, err
	}
	file, err := os.OpenFile(filepath.Join(root, ".install.lock"), os.O_RDWR|os.O_CREATE, 0600)
	if err != nil {
		return nil, err
	}
	deadline := time.NewTimer(30 * time.Second)
	defer deadline.Stop()
	for {
		if err = tryRuntimeLock(file); err == nil {
			return file, nil
		} else if !errors.Is(err, errRuntimeLocked) {
			file.Close()
			return nil, err
		}
		select {
		case <-ctx.Done():
			file.Close()
			return nil, ctx.Err()
		case <-deadline.C:
			file.Close()
			return nil, errors.New(i18n.Message56b7250a0f980d75)
		case <-time.After(100 * time.Millisecond):
		}
	}
}

var errRuntimeLocked = errors.New("Runtime installation locked")
