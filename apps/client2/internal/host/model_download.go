package host

import "respire-client2/internal/i18n"

// Multi-threaded downloads for BGE-M3 weights: the client fetches the pinned
// files itself with parallel Range chunks and SHA256 verification.

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"time"
)

const (
	modelRepo     = "Xenova/bge-m3"
	modelRevision = "4de13258303883538bd53b696b452bf8099f0858"
	tokenizerSHA  = "6710678b12670bc442b99edc952c4d996ae309a7020c1fa0096dd245c2faf790"
)

type modelFileSpec struct {
	rel    string
	sha256 string
	size   int64
}

var modelFileSpecs = map[string]modelFileSpec{
	"tokenizer": {rel: "tokenizer.json", sha256: tokenizerSHA, size: 17082821},
	"quantized": {rel: "onnx/model_quantized.onnx", sha256: "0826f8c1ab9edf1801db86c61919d4d108e8bfc0b809ec823ad366882ff0b77d", size: 569694530},
	"fp16":      {rel: "onnx/model_fp16.onnx", sha256: "4f1a646a3d4f39985589e9991a717044ede8278617fe55e3d246838bc05055e9", size: 1133992936},
	"int8":      {rel: "onnx/model_int8.onnx", sha256: "a206e10e995aa2a833924bcd725ba5dd6c3425cd34bac3cf2b5677cd2a1c51d6", size: 568456694},
	"uint8":     {rel: "onnx/model_uint8.onnx", sha256: "3222211565d0d5da2fe89e0c40856ed879e5c5dd3369fbbe685213469498e811", size: 568456765},
	"q4":        {rel: "onnx/model_q4.onnx", sha256: "4f6d9786712f8dbf00efbb08ffd819395ef05d860249e852b4efc16022f753a6", size: 1248237611},
	"q4f16":     {rel: "onnx/model_q4f16.onnx", sha256: "8305fdfa48b5702ecb4a6d84c1a52a4d1cb27d69880ff7f69d1a93d795864f25", size: 699932840},
	"bnb4":      {rel: "onnx/model_bnb4.onnx", sha256: "3781d63fa70c05bd65e1d4ed66ca48933a36b3ff7c75b89eea051d8d625cc093", size: 1229364419},
	"fp32":      {rel: "onnx/model.onnx", sha256: "5d89a0010dd39aa2cfa8b22bb49f06904c5bbf5877135f877da419480f40cde3", size: 607298},
}

var m3ExternalData = modelFileSpec{
	rel: "onnx/model.onnx_data", size: 2266820608,
	sha256: "1eebfb28493f67bba03ce0ef64bfdc7fc5a3bd9d7493f818bb1d78cd798416b4",
}

const m3CPUContainerURL = "https://github.com/risense-ai/respire-cli/releases/download/model-bge-m3-ort-v1/model_quantized.cpu-v1.ort"

var m3CPUContainer = modelFileSpec{
	rel: "onnx/model_quantized.cpu-v1.ort", size: 569654128,
	sha256: "7ff152e912bc234f5c90e4012f99897a7b2ce180f016db2321304057756b64ff",
}

var modelMirrors = []string{"https://hf-mirror.com", "https://hf-mirror.net", "https://huggingface.co"}

var errCorruptModelCleanup = errors.New(i18n.Messaged3998e4b9ef5bf00)

// ModelDir matches the CLI's m3_model_dir(): RSRS_DATA_DIR (already resolved
// into DataRoot) or ~/.rsrs, plus models/bge-m3.
func (m *Manager) ModelDir() string {
	return filepath.Join(m.DataRoot, "models", "bge-m3")
}

// modelDirFor resolves the weight directory for one catalog model: profile
// models live under models/<id>; m3 keeps the legacy resolution.
func (m *Manager) modelDirFor(model string) (string, modelDefinition, error) {
	definition, err := modelDefinitionByID(model)
	if err != nil {
		return "", definition, err
	}
	if definition.Legacy {
		return m.ModelDir(), definition, nil
	}
	return m.ProfileDir(model), definition, nil
}

// modelClient streams multi-hundred-MiB chunks: no overall deadline (the
// request context governs). It shares the manager's outbound transport, so
// environment and system proxy settings apply to model downloads too.
func (m *Manager) modelClient() *http.Client {
	if m.modelHTTPClient != nil {
		return m.modelHTTPClient
	}
	transport := http.DefaultTransport.(*http.Transport).Clone()
	if m.proxy != nil {
		transport.Proxy = m.proxy.proxyFunc(func() string { return m.Preferences().ProxyMode })
	}
	return &http.Client{Transport: transport}
}

func modelOrigins(mirror string) []string {
	if mirror == "auto" {
		return modelMirrors
	}
	return []string{mirror}
}

func modelFileURL(origin, rel string) string {
	return modelFileURLOf(modelDefinition{Repo: modelRepo, Revision: modelRevision}, origin, rel)
}

func modelFileURLOf(definition modelDefinition, origin, rel string) string {
	// rel comes from filepath.Join (variantFileName) and carries \ on Windows;
	// the resolve endpoint expects a slash-separated repo path.
	return origin + "/" + definition.Repo + "/resolve/" + definition.Revision + "/" + filepath.ToSlash(rel)
}

func (m *Manager) modelMirrorSetting(explicit string) string {
	if strings.TrimSpace(explicit) != "" {
		return explicit
	}
	return m.Preferences().ModelMirror
}

func expandTilde(p string) (string, error) {
	if rest, ok := strings.CutPrefix(p, "~/"); ok {
		home, err := os.UserHomeDir()
		if err != nil {
			return "", err
		}
		return filepath.Join(home, rest), nil
	}
	return p, nil
}

// DownloadModel fetches one pinned file into the model's dir. For m3 the
// variant is one of the legacy keys or "baseline" (CPU first-run files);
// profile models download tokenizer.json plus
// the chosen onnx variant from their pinned repository.
func (m *Manager) DownloadModel(ctx context.Context, model, variant, mirror string) error {
	key, err := downloadVariant(model, variant)
	if err != nil {
		return err
	}
	m.modelMu.Lock()
	reserved := m.reserveModelDownload(key)
	m.modelMu.Unlock()
	if !reserved {
		return ErrModelVariantBusy
	}
	defer m.releaseModelDownload(key)
	return m.downloadModel(ctx, model, variant, mirror, true)
}

func (m *Manager) StartModelDownload(model, variant, mirror string) (string, error) {
	if model == "" {
		model = DefaultModelID
	}
	key, err := downloadVariant(model, variant)
	if err != nil {
		return "", err
	}
	mirror, err = NormalizeModelMirror(m.modelMirrorSetting(mirror))
	if err != nil {
		return "", fmt.Errorf("%w: %w", ErrInvalidModelVariant, err)
	}
	m.modelMu.Lock()
	defer m.modelMu.Unlock()
	if !m.reserveModelDownload(key) {
		return "", ErrModelVariantBusy
	}
	id := m.StartDownload(i18n.Message446478c6a451bc64+key.Variant, func(ctx context.Context) error {
		defer m.releaseModelDownload(key)
		ctx, cancel := context.WithTimeout(ctx, 60*time.Minute)
		defer cancel()
		return m.downloadModel(ctx, model, variant, mirror, false)
	})
	if id == "" {
		delete(m.modelDownloads, key)
		return "", ErrModelVariantBusy
	}
	return id, nil
}

func downloadVariant(model, variant string) (modelVariant, error) {
	if variant == "baseline" {
		variant = ""
	}
	_, variant, _, err := selectedModelVariant(model, variant)
	return modelVariant{Model: model, Variant: variant}, err
}

func (m *Manager) reserveModelDownload(key modelVariant) bool {
	if m.modelDownloads[key] {
		return false
	}
	if m.modelDownloads == nil {
		m.modelDownloads = make(map[modelVariant]bool)
	}
	m.modelDownloads[key] = true
	return true
}

func (m *Manager) releaseModelDownload(key modelVariant) {
	m.modelMu.Lock()
	defer m.modelMu.Unlock()
	delete(m.modelDownloads, key)
}

func (m *Manager) downloadModel(ctx context.Context, model, variant, mirror string, runtimeManaged bool) error {
	release, err := m.holdProfile()
	if err != nil {
		return err
	}
	defer release()
	dir, definition, err := m.modelDirFor(model)
	if err != nil {
		return err
	}
	baseline := variant == "baseline"
	if baseline {
		variant = ""
	}
	variant = definition.selectedVariant(variant)
	weights, err := definition.weightFiles(variant)
	if err != nil {
		return err
	}
	mirror, err = NormalizeModelMirror(m.modelMirrorSetting(mirror))
	if err != nil {
		return err
	}
	origins := modelOrigins(mirror)
	if runtimeManaged && definition.Legacy && variant == "quantized" && !baseline {
		health, supported, err := m.runtimeModelInstallCapability(ctx)
		if err != nil {
			return err
		}
		if supported {
			return m.downloadManagedM3(ctx, health, definition, origins)
		}
	}
	files := append([]modelFileSpec{definition.Tokenizer}, weights...)
	if err := m.downloadModelFiles(ctx, definition, dir, files, origins); err != nil {
		return err
	}
	if definition.Legacy && baseline && m3UsesCPUContainer() {
		return m.downloadModelFrom(ctx, m3CPUContainerURL,
			filepath.Join(m.ModelDir(), filepath.FromSlash(m3CPUContainer.rel)), m3CPUContainer)
	}
	return nil
}

func (m *Manager) downloadModelFiles(ctx context.Context, definition modelDefinition, dir string, files []modelFileSpec, origins []string) error {
	for _, spec := range files {
		dest := filepath.Join(dir, filepath.FromSlash(spec.rel))
		if modelFileValid(dest, spec) {
			continue
		}
		var lastErr error
		for _, origin := range origins {
			lastErr = m.downloadModelFrom(ctx, modelFileURLOf(definition, origin, spec.rel), dest, spec)
			if lastErr == nil {
				break
			}
			if errors.Is(lastErr, errCorruptModelCleanup) {
				return lastErr
			}
			if ctx.Err() != nil {
				return ctx.Err()
			}
		}
		if lastErr != nil {
			return lastErr
		}
	}
	return nil
}

func m3UsesCPUContainer() bool {
	if choice, set := os.LookupEnv("RSRS_CPU_OWNED_ORT_EXPERIMENT"); set && choice == "0" {
		return false
	}
	_, profiling := os.LookupEnv("RSRS_ORT_PROFILE")
	return !profiling
}

func (m *Manager) cpuContainerPresent(dir string) bool {
	if !m3UsesCPUContainer() {
		return true
	}
	relative := filepath.FromSlash(m3CPUContainer.rel)
	// Runtime resolves its user cache ahead of an explicitly configured model directory.
	info, err := os.Lstat(filepath.Join(m.ModelDir(), relative))
	if errors.Is(err, os.ErrNotExist) {
		info, err = os.Lstat(filepath.Join(dir, relative))
	}
	return err == nil && info.Mode().IsRegular() && info.Size() == m3CPUContainer.size
}

func modelFileValid(path string, spec modelFileSpec) bool {
	info, err := os.Stat(path)
	return err == nil && info.Mode().IsRegular() && info.Size() == spec.size && fileHashMust(path) == spec.sha256
}

func fileHashMust(path string) string {
	sum, err := fileHash(path)
	if err != nil {
		return ""
	}
	return sum
}

// downloadModelFrom downloads url into dest through parallel Range chunks
// (falling back to a single stream when the source ignores Range). Partial
// chunks stay on disk so a retry resumes instead of restarting.
func (m *Manager) downloadModelFrom(ctx context.Context, url, dest string, spec modelFileSpec) error {
	if modelFileValid(dest, spec) {
		return nil
	}
	if err := os.MkdirAll(filepath.Dir(dest), 0700); err != nil {
		return err
	}
	tmp := dest + ".download"
	workers := 8
	const minParallel = 32 << 20
	if spec.size < minParallel {
		workers = 1
	}
	chunk := (spec.size + int64(workers) - 1) / int64(workers)
	parts := make([]string, workers)
	for i := range parts {
		parts[i] = fmt.Sprintf("%s.p%d", tmp, i)
	}
	if err := m.downloadChunks(ctx, url, tmp, parts, spec, chunk, workers); err != nil {
		return err
	} // Merge, verify, publish.
	out, err := os.OpenFile(tmp, os.O_CREATE|os.O_TRUNC|os.O_WRONLY, 0600)
	if err != nil {
		return err
	}
	hasher := sha256.New()
	writer := io.MultiWriter(out, hasher)
	var merged int64
	for _, part := range parts {
		input, err := os.Open(part)
		if err != nil {
			out.Close()
			return err
		}
		size, err := io.Copy(writer, input)
		input.Close()
		if err != nil {
			out.Close()
			return err
		}
		merged += size
	}
	if err = out.Close(); err != nil {
		return err
	}
	if merged != spec.size || hex.EncodeToString(hasher.Sum(nil)) != spec.sha256 {
		verifyErr := fmt.Errorf(i18n.Messagedbc2929d5bdbdbb3, filepath.Base(dest))
		cleanupErr := os.Remove(tmp)
		for _, part := range parts {
			cleanupErr = errors.Join(cleanupErr, os.Remove(part))
		}
		if cleanupErr != nil {
			return errors.Join(verifyErr, fmt.Errorf("%w: %w", errCorruptModelCleanup, cleanupErr))
		}
		return verifyErr
	}
	if err = os.Rename(tmp, dest); err != nil {
		return err
	}
	for _, part := range parts {
		_ = os.Remove(part)
	}
	m.progress(i18n.Messagee960ec73108e5304, 0, 0)
	return nil
}

func (m *Manager) downloadChunks(ctx context.Context, url, tmp string, parts []string, spec modelFileSpec, chunk int64, workers int) error {
	var done atomic.Int64
	errs := make([]error, workers)
	base := filepath.Base(spec.rel)
	var wg sync.WaitGroup
	for i := 0; i < workers; i++ {
		start := int64(i) * chunk
		end := start + chunk - 1
		if end >= spec.size {
			end = spec.size - 1
		}
		if start > end {
			break
		}
		wg.Add(1)
		go func(i int, start, end int64) {
			defer wg.Done()
			errs[i] = m.downloadChunk(ctx, url, parts[i], base, start, end, &done, spec.size)
		}(i, start, end)
	}
	wg.Wait()
	for _, err := range errs {
		if err != nil {
			return err
		}
	}
	return nil
}

// downloadChunk fetches one Range into its part file, resuming from the part's
// current size and retrying with fresh requests until complete.
func (m *Manager) downloadChunk(ctx context.Context, url, part, base string, start, end int64, done *atomic.Int64, total int64) error {
	partTotal := end - start + 1
	var have int64
	if info, err := os.Stat(part); err == nil {
		have = info.Size()
	}
	if have > partTotal {
		_ = os.Remove(part)
		have = 0
	}
	if have >= partTotal {
		done.Add(partTotal)
		return nil
	}
	done.Add(have)
	file, err := os.OpenFile(part, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0600)
	if err != nil {
		return err
	}
	defer file.Close()
	var lastReport int64
	report := func() {
		if now := done.Load(); now-lastReport >= 8<<20 || now == total {
			lastReport = now
			m.progress(i18n.Message6c643d63d73835e3+base, now, total)
		}
	}
	for attempt := 0; attempt < 6; attempt++ {
		if ctx.Err() != nil {
			return ctx.Err()
		}
		offset := start + have
		req, err := http.NewRequestWithContext(ctx, "GET", url, nil)
		if err != nil {
			return err
		}
		req.Header.Set("Range", fmt.Sprintf("bytes=%d-%d", offset, end))
		res, err := m.modelClient().Do(req)
		if err != nil {
			continue
		}
		if res.StatusCode == 200 && partTotal != total {
			// Source ignored Range: only safe as a single-stream whole file.
			res.Body.Close()
			return errors.New(i18n.Message5cc087a37a20139d)
		}
		if res.StatusCode != 206 && res.StatusCode != 200 {
			res.Body.Close()
			continue
		}
		buf := make([]byte, 256<<10)
		for {
			n, readErr := res.Body.Read(buf)
			if n > 0 {
				if _, writeErr := file.Write(buf[:n]); writeErr != nil {
					res.Body.Close()
					return writeErr
				}
				have += int64(n)
				done.Add(int64(n))
				report()
			}
			if readErr == io.EOF {
				res.Body.Close()
				if have >= partTotal {
					return nil
				}
				break // short body: retry from current offset
			}
			if readErr != nil {
				res.Body.Close()
				break
			}
		}
	}
	if have >= partTotal {
		return nil
	}
	return fmt.Errorf(i18n.Messagea341ab0aaa2df53b, have, partTotal)
}
