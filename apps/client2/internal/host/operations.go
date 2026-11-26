package host

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"respire-client2/internal/i18n"
	"sort"
	"strings"
	"time"
)

type Operation struct {
	ID              string            `json:"id"`
	Name            string            `json:"name"`
	State           string            `json:"state"`
	Started         time.Time         `json:"started"`
	Finished        *time.Time        `json:"finished,omitempty"`
	Error           string            `json:"error,omitempty"`
	Envelope        json.RawMessage   `json:"envelope,omitempty"`
	Scope           *runtimeScope     `json:"scope,omitempty"`
	RuntimeID       string            `json:"runtimeOperationId,omitempty"`
	Transaction     *hostTransaction  `json:"transaction,omitempty"`
	Input           *hostInputRequest `json:"input,omitempty"`
	TransportID     string            `json:"transportId,omitempty"`
	MaterialSaved   bool              `json:"materialSaved,omitempty"`
	MaterialPending bool              `json:"materialPending,omitempty"`
	write, host     bool
	inputBlocked    bool
	keygenRecovery  string
	coreChange      *coreConfigChange
}

type operationRecord struct {
	ID            string            `json:"id"`
	Name          string            `json:"name"`
	Started       time.Time         `json:"started"`
	State         string            `json:"state"`
	Host          bool              `json:"host"`
	Scope         *runtimeScope     `json:"scope,omitempty"`
	RuntimeID     string            `json:"runtimeOperationId,omitempty"`
	Transaction   *hostTransaction  `json:"transaction,omitempty"`
	TransportID   string            `json:"transportId,omitempty"`
	MaterialSaved bool              `json:"materialSaved,omitempty"`
	CoreChange    *coreConfigChange `json:"coreConfig,omitempty"`
}

type rpcResponse struct {
	V           int             `json:"v"`
	ID          string          `json:"id"`
	OK          bool            `json:"ok"`
	Exit        int             `json:"exit"`
	Error       string          `json:"error"`
	Code        string          `json:"code"`
	Envelope    json.RawMessage `json:"envelope"`
	Scope       *runtimeScope   `json:"scope,omitempty"`
	OperationID string          `json:"operation_id,omitempty"`
	DataDir     string          `json:"data_dir,omitempty"`
}

var operationID = regexp.MustCompile(`^desktop-[0-9a-f-]{36}$`)

func (m *Manager) loadOperations() error {
	m.jobs = map[string]*Operation{}
	b, err := os.ReadFile(filepath.Join(m.Root, "requests.json"))
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	if err != nil {
		return err
	}
	var records []operationRecord
	if err = json.Unmarshal(b, &records); err != nil {
		return fmt.Errorf(i18n.Messagebb380ac7caaa944d, err)
	}
	for _, r := range records {
		if !operationID.MatchString(r.ID) {
			return errors.New(i18n.Messagec630435b8f1f9acd)
		}
		state := r.State
		if state == "running" {
			state = "unconfirmed"
		}
		m.jobs[r.ID] = &Operation{ID: r.ID, Name: r.Name, Started: r.Started, State: state, host: r.Host, write: true, Scope: r.Scope, RuntimeID: r.RuntimeID, Transaction: r.Transaction, TransportID: r.TransportID, MaterialSaved: r.MaterialSaved, coreChange: r.CoreChange}
	}
	return nil
}

func (m *Manager) persistOperationsLocked() error {
	release, err := m.holdProfile()
	if err != nil {
		return err
	}
	defer release()
	var records []operationRecord
	for _, j := range m.jobs {
		if j.write {
			records = append(records, operationRecord{ID: j.ID, Name: j.Name, Started: j.Started, State: j.State, Host: j.host, Scope: j.Scope, RuntimeID: j.RuntimeID, Transaction: j.Transaction, TransportID: j.TransportID, MaterialSaved: j.MaterialSaved, CoreChange: j.coreChange})
		}
	}
	sort.Slice(records, func(i, j int) bool { return records[i].Started.After(records[j].Started) })
	return writeJSON(filepath.Join(m.Root, "requests.json"), records)
}

func (m *Manager) Operations() []Operation {
	m.mu.Lock()
	defer m.mu.Unlock()
	list := make([]Operation, 0, len(m.jobs))
	for _, j := range m.jobs {
		copy := *j
		copy.Input = pendingHostInput(copy)
		copy.MaterialPending = copy.materialPending()
		copy.Envelope = nil
		list = append(list, copy)
	}
	sort.Slice(list, func(i, j int) bool { return list[i].Started.After(list[j].Started) })
	return list
}

func (m *Manager) Operation(id string) (Operation, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	job, ok := m.jobs[id]
	if !ok {
		return Operation{}, errors.New(i18n.Message1365397b075962ba)
	}
	copy := *job
	if job.keygenRecovery != "" {
		var envelope map[string]json.RawMessage
		if err := json.Unmarshal(copy.Envelope, &envelope); err != nil {
			return Operation{}, err
		}
		details, err := json.Marshal(map[string]string{"super": job.keygenRecovery})
		if err != nil {
			return Operation{}, err
		}
		defer clear(details)
		envelope["details"] = details
		copy.Envelope, err = json.Marshal(envelope)
		if err != nil {
			return Operation{}, err
		}
		job.keygenRecovery, copy.keygenRecovery = "", ""
	}
	copy.Input = pendingHostInput(copy)
	copy.MaterialPending = copy.materialPending()
	return copy, nil
}

func (m *Manager) HasActiveOperations() bool {
	m.mu.Lock()
	defer m.mu.Unlock()
	for _, j := range m.jobs {
		if j.State == "running" {
			return true
		}
	}
	return false
}

func (m *Manager) Acknowledge(id string) error {
	m.mu.Lock()
	defer m.mu.Unlock()
	job, ok := m.jobs[id]
	if !ok {
		return nil
	}
	if job.State == "running" || job.State == "unconfirmed" {
		return errors.New(i18n.Message510959201c07e24a)
	}
	if job.Name == "space_invite" {
		job.Envelope = nil
	}
	if !job.write {
		delete(m.jobs, id)
	}
	return nil
}

func (m *Manager) RuntimeStatus(ctx context.Context) (json.RawMessage, error) {
	scope, err := m.requestScope(ctx)
	if err != nil {
		return nil, err
	}
	id := "status-" + fmt.Sprint(time.Now().UnixNano())
	reply, err := m.rpcResponse(ctx, "cli.exec", []string{"--json", "status"}, id, scope)
	if err != nil {
		return m.inspectResult(ctx, id, scope, err)
	}
	if reply.Scope == nil || *reply.Scope != *scope || reply.OperationID != id {
		return nil, &unconfirmedError{id, errors.New(i18n.Messagefb9a457abcc23607)}
	}
	var status struct {
		Command string `json:"command"`
		Status  string `json:"status"`
		Summary struct {
			CredentialState string `json:"credential_state"`
			StoreReady      *bool  `json:"store_ready"`
		} `json:"summary"`
		Errors []string `json:"errors"`
	}
	// A locked store is a valid read-only status, but other RPC warnings still fail.
	if !reply.OK && reply.Exit == 2 && reply.Code == "" && reply.Error == "" &&
		json.Unmarshal(reply.Envelope, &status) == nil && status.Command == "status" &&
		status.Status == "warn" && status.Summary.CredentialState == "required" &&
		status.Summary.StoreReady != nil && !*status.Summary.StoreReady && len(status.Errors) == 0 {
		return reply.Envelope, nil
	}
	return reply.Envelope, responseError(reply)
}

func (m *Manager) Submit(name, id string, raw json.RawMessage, confirmed bool, secret string) (Operation, error) {
	if !operationID.MatchString(id) {
		return Operation{}, errors.New(i18n.Message4c69a0952a285e6c)
	}
	spec, ok := operationSpec(name)
	if !ok {
		return Operation{}, errors.New(i18n.Messageeee8760c4f458b6c)
	}
	if name == "migration_apply" {
		if err := m.requireTakeoverProfile(); err != nil {
			return Operation{}, err
		}
	}
	if spec.write && !confirmed {
		return Operation{}, errors.New(i18n.Messagec7e0be86a2792c5e)
	}
	if secret != "" && name != "classification_save" && !strings.HasPrefix(spec.method, "auth.") {
		return Operation{}, errors.New(i18n.Message55f4aed7eef1e9e5)
	}
	argv, err := spec.build(raw)
	if err != nil {
		return Operation{}, err
	}
	if name == "agent_set" {
		var input struct {
			Setting string `json:"setting"`
		}
		if err := json.Unmarshal(raw, &input); err != nil {
			return Operation{}, err
		}
		key, _, _ := strings.Cut(input.Setting, "=")
		if key = strings.TrimSpace(key); key == "read_heat" || key == "recall_fusion" {
			if !hostOperationID.MatchString(strings.TrimPrefix(id, "desktop-")) {
				return Operation{}, errors.New(i18n.Message74792dc0c48ac25b)
			}
			spec.method, spec.host = "desktop.core-config", false
		} else if isDesktopAgentTextSetting(key) {
			spec.method, spec.host = "desktop.agent-text-setting", false
		} else if isWorkspaceModeSetting(key) {
			spec.method = "desktop.workspace-setting"
		} else {
			return Operation{}, errors.New(i18n.Message668b798392208826)
		}
	}
	if name == "model_progress" {
		argv = append(argv, "false")
	}
	if name == "model_cancel" {
		argv = append(argv, "true")
	}
	m.mu.Lock()
	if previous, exists := m.jobs[id]; exists {
		copy := *previous
		m.mu.Unlock()
		if previous.Name != name {
			return Operation{}, errors.New(i18n.Message8d852a8ad9c3067d)
		}
		return copy, nil
	}
	active := 0
	var oldest *Operation
	for key, j := range m.jobs {
		if j.State == "running" {
			active++
		} else if j.State != "unconfirmed" && !j.materialPending() && (oldest == nil || j.Started.Before(oldest.Started)) {
			oldest = m.jobs[key]
		}
	}
	if active >= 8 {
		m.mu.Unlock()
		return Operation{}, errors.New(i18n.Messagee0277a8694759118)
	}
	if len(m.jobs) >= 128 {
		if oldest == nil {
			m.mu.Unlock()
			return Operation{}, errors.New(i18n.Message8d27fa7b9e9394f6)
		}
		delete(m.jobs, oldest.ID)
	}
	job := &Operation{ID: id, Name: name, State: "running", Started: time.Now().UTC(), host: spec.host, write: spec.write}
	m.jobs[id] = job
	if job.write {
		if err = m.persistOperationsLocked(); err != nil {
			delete(m.jobs, id)
			m.mu.Unlock()
			return Operation{}, err
		}
	}
	copy := *job
	m.mu.Unlock()
	go m.runOperation(spec, argv, secret, id)
	return copy, nil
}

func (m *Manager) runOperation(spec commandSpec, args []string, secret, id string) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	// Account operations wait for human authorization without a wall-clock deadline.
	if !strings.HasPrefix(spec.method, "auth.") {
		timedCtx, timedCancel := context.WithTimeout(ctx, 30*time.Minute)
		defer timedCancel()
		ctx = timedCtx
	}
	var envelope json.RawMessage
	var keygenRecovery string
	var err error
	switch {
	case spec.method == "model.apply-engine":
		envelope, err = applyEngine(ctx, m, args, id)
	case spec.method == "desktop.device-config":
		envelope, err = m.desktopDeviceConfig(ctx, deviceConfigIntent{})
	case spec.method == "desktop.server-set":
		envelope, err = m.desktopDeviceConfig(ctx, deviceConfigIntent{Addr: &args[0]})
	case spec.method == "desktop.autosync-set":
		enabled := args[0] == "true"
		envelope, err = m.desktopDeviceConfig(ctx, deviceConfigIntent{Autosync: &enabled})
	case spec.method == "desktop.cure-auto-set":
		enabled := args[0] == "true"
		envelope, err = m.desktopDeviceConfig(ctx, deviceConfigIntent{CureAuto: &enabled})
	case spec.method == "desktop.data-dir-set":
		envelope, err = m.desktopDeviceConfig(ctx, deviceConfigIntent{DataDir: &args[0]})
	case spec.method == "desktop.agent-config":
		envelope, err = m.desktopAgentSettings()
	case spec.method == "desktop.classification", spec.method == "desktop.classification-save":
		envelope, err = m.desktopClassificationConfig(ctx, args, secret, spec.method == "desktop.classification-save")
	case spec.method == "desktop.core-config":
		envelope, err = m.setDesktopCoreConfig(ctx, id, args[0])
	case spec.method == "desktop.keys-export":
		envelope, err = m.desktopKeysExport(ctx, args)
	case spec.method == "desktop.account-list", spec.method == "desktop.space-list":
		envelope, err = m.desktopProfileList(spec.method == "desktop.space-list")
	case spec.method == "desktop.space-members":
		envelope, err = m.desktopSpaceMembers(args)
	case spec.method == "desktop.migration-list":
		envelope, err = m.desktopLegacyProfiles()
	case spec.method == "desktop.migration-apply":
		envelope, err = m.desktopLegacyMigration(ctx, id, args, false)
	case spec.method == "desktop.account-use", spec.method == "desktop.space-use":
		envelope, err = m.desktopProfileSwitch(ctx, args[0], spec.method == "desktop.space-use")
	case spec.method == "desktop.space-create":
		envelope, err = m.desktopSpaceCreate(ctx, args[0])
	case spec.method == "desktop.offline-keygen":
		envelope, keygenRecovery, err = m.desktopOfflineKeygen(ctx, id, false)
	case spec.method == "auth.login", spec.method == "auth.login.browser", spec.method == "auth.register":
		envelope, err = m.desktopRuntimeAccountLogin(ctx, args, secret, id, spec.method)
	case spec.method == "desktop.logout", spec.method == "desktop.logout-full":
		envelope, err = m.desktopRuntimeLogout(ctx, id, spec.method == "desktop.logout-full", false)
	case spec.method == "auth.recover", spec.method == "auth.reset":
		envelope, err = m.desktopRuntimeKeyRecovery(ctx, args, secret, id, spec.method)
	case spec.method == "desktop.agent-text-setting":
		envelope, err = m.setDesktopAgentTextSetting(ctx, id, args[0])
	case spec.method == "desktop.workspace-setting":
		envelope, err = m.setDesktopWorkspaceMode(ctx, args[0])
	case spec.method == "desktop.model-uninstall":
		envelope, err = m.uninstallM3(ctx)
	case spec.method == "desktop.space-kick":
		envelope, err = m.desktopSpaceMembership(ctx, id, args, "kick", "", hostCredentialStore{})
	case spec.method == "desktop.space-invite":
		envelope, err = m.desktopSpaceMembership(ctx, id, args, "invite", "", nativeRuntimeCredentialStore(ctx))
	case spec.method == "desktop.space-remove":
		envelope, err = m.removeDesktopSpace(ctx, id, args[0], false)
	case spec.method == "desktop.space-join":
		envelope, err = m.desktopSpaceMembership(ctx, id, args, "join", "", nativeRuntimeCredentialStore(ctx))
	case spec.method == "desktop.install-engines":
		envelope, err = m.installModelEngines(ctx)
	case spec.host:
		err = errors.New(i18n.Message6fe3d2196bba4844)
	default:
		method := spec.method
		if method == "" {
			method = "cli.exec"
			args = append([]string{"--json"}, args...)
		}
		envelope, err = m.executeRPC(ctx, method, args, id)
	}
	m.mu.Lock()
	defer m.mu.Unlock()
	job := m.jobs[id]
	if spec.host {
		returned := transactionFromEnvelope(job.Name, envelope)
		if job.Transaction != nil && job.Transaction.RequestBound {
			if returned == nil && (len(envelope) > 0 || err == nil) {
				var rejection struct {
					Status  string `json:"status"`
					Summary struct {
						Reason string `json:"reason"`
					} `json:"summary"`
				}
				if json.Unmarshal(envelope, &rejection) != nil || rejection.Status != "fail" || rejection.Summary.Reason != "invalid_input" {
					err = &unconfirmedError{id, errors.Join(err, errors.New(i18n.Message59de7e939e8e4627))}
				}
			} else if returned != nil {
				if !job.Transaction.acceptsCoreResult(returned) {
					err = &unconfirmedError{id, errors.New(i18n.Message5bf7222fdabc8abe)}
				} else {
					returned.RequestBound = true
					job.Transaction = returned
				}
			}
		} else if job.Transaction != nil && (job.Transaction.RuntimeLogin || job.Transaction.RuntimeKey) {
			if returned != nil && *returned != *job.Transaction {
				err = &unconfirmedError{id, errors.New(i18n.Messageb526277c1295cec0)}
			}
		} else {
			job.Transaction = returned
		}
	}
	finished := time.Now().UTC()
	job.Finished = &finished
	job.Envelope = envelope
	job.keygenRecovery = keygenRecovery
	job.State = "done"
	if err != nil {
		job.State = "failed"
		var unknown *unconfirmedError
		if errors.As(err, &unknown) {
			unknown.id = id
			job.State = "unconfirmed"
		}
		job.Error = err.Error()
	}
	if job.write {
		if saveErr := m.persistOperationsLocked(); saveErr != nil {
			job.Error = errors.Join(err, fmt.Errorf(i18n.Message4cde63a7c11d75a7, saveErr)).Error()
		}
	}
}

type unconfirmedError struct {
	id    string
	cause error
}

func (e *unconfirmedError) Error() string {
	return fmt.Sprintf(i18n.Message52e7414d84ce5ede, e.id, e.cause)
}

func (e *unconfirmedError) Unwrap() error { return e.cause }

func envelopeError(raw json.RawMessage) error {
	var result struct {
		Status string   `json:"status"`
		Errors []string `json:"errors"`
	}
	if err := json.Unmarshal(raw, &result); err != nil {
		return err
	}
	if result.Status == "" {
		return errors.New(i18n.Message45c2dbf8b0b66877)
	}
	if result.Status == "pending" {
		return &unconfirmedError{cause: errors.New(i18n.Messagefb89b800e9f14f2e)}
	}
	if result.Status == "fail" {
		if len(result.Errors) > 0 {
			return errors.New(result.Errors[0])
		}
		return errors.New(i18n.Message0370039d5646df2f)
	}
	return nil
}

func (m *Manager) rpcResponse(ctx context.Context, method string, args []string, id string, scope *runtimeScope) (rpcResponse, error) {
	raw, err := m.legacyRPCRequest(ctx, map[string]any{"v": 1, "id": id, "method": method, "args": args, "scope": scope})
	var reply rpcResponse
	if err != nil {
		return reply, err
	}
	if err = json.Unmarshal(raw, &reply); err != nil {
		return reply, err
	}
	if reply.V != 1 || reply.ID != id {
		return reply, errors.New(i18n.Messagef8fe33f1f9f7d5f3)
	}
	return reply, nil
}

func (m *Manager) executeRPC(ctx context.Context, method string, args []string, id string) (json.RawMessage, error) {
	scope, err := m.requestScope(ctx)
	if err != nil {
		return nil, err
	}
	return m.executeRPCScoped(ctx, method, args, id, scope)
}

func (m *Manager) executeRPCScoped(ctx context.Context, method string, args []string, id string, scope *runtimeScope) (json.RawMessage, error) {
	parent, _, _ := strings.Cut(id, "/")
	preview := false
	m.mu.Lock()
	if job := m.jobs[parent]; job != nil {
		preview = id == parent && job.Name == "organize_resort_preview" && !job.write && !job.host
		job.Scope, job.RuntimeID = scope, id
		if job.write {
			if err := m.persistOperationsLocked(); err != nil {
				m.mu.Unlock()
				return nil, err
			}
		}
	}
	m.mu.Unlock()
	reply, err := m.rpcResponse(ctx, method, args, id, scope)
	if err != nil {
		return m.inspectResult(ctx, id, scope, err)
	}
	if legacyRPCUsesReceipt(method) && (reply.Scope == nil || *reply.Scope != *scope || reply.OperationID != id) {
		return nil, &unconfirmedError{id, errors.New(i18n.Messagefb9a457abcc23607)}
	}
	if preview && completedOrganizePreview(id, scope, method, args, reply) {
		return reply.Envelope, nil
	}
	return reply.Envelope, responseError(reply)
}

func responseError(reply rpcResponse) error {
	if !reply.OK && reply.Error != "" {
		return errors.New(reply.Error)
	}
	if err := envelopeError(reply.Envelope); err != nil {
		return err
	}
	if !reply.OK {
		return errors.New(i18n.Message1771eb5cfc51b547)
	}
	return nil
}
