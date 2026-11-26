package host

import (
	"encoding/binary"
	"errors"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"unicode/utf16"
	"unsafe"

	"golang.org/x/sys/windows"
)

var errProfileScope = errors.New("configured profile has no trusted local directory identity")

// The ordinary path keeps its existing missing-suffix semantics. Only a raw,
// explicitly selected, absolute local path can request junction admission.
func admitProfile(root, configured string) (profileAdmission, func(), error) {
	if configured != "" && strings.HasPrefix(filepath.ToSlash(windowsDOSPath(configured)), "//") {
		return profileAdmission{}, nil, errProfileScope
	}
	path, err := startupScopePath(root)
	if err == nil {
		return profileAdmission{root: path}, func() {}, nil
	}
	if !plainProfilePath(configured) || !samePath(root, configured) {
		return profileAdmission{}, nil, err
	}
	pins, err := pinConfiguredProfile(configured)
	if err != nil {
		return profileAdmission{}, nil, err
	}
	// Keep an identity handle (with delete sharing) between operations. An
	// unlinked object's file ID cannot be recycled while this handle exists.
	identity, err := openProfileDirectory(pins.root, windows.FILE_SHARE_READ|windows.FILE_SHARE_WRITE|windows.FILE_SHARE_DELETE)
	if err != nil {
		pins.close()
		return profileAdmission{}, nil, err
	}
	expected := pins.root
	reopen := func() (func(), error) {
		now, err := pinConfiguredProfile(configured)
		if err != nil {
			return nil, err
		}
		before, e1 := identity.Stat()
		after, e2 := now.files[len(now.files)-1].Stat()
		if e1 != nil || e2 != nil || !samePath(now.root, expected) || !os.SameFile(before, after) {
			now.close()
			return nil, errProfileScope
		}
		return now.close, nil
	}
	return profileAdmission{root: expected, reopen: reopen}, pins.close, nil
}

func plainProfilePath(path string) bool {
	if len(path) < 4 || path[1:3] != ":\\" || !filepath.IsAbs(path) || filepath.Clean(path) != path ||
		!(path[0] >= 'A' && path[0] <= 'Z' || path[0] >= 'a' && path[0] <= 'z') {
		return false
	}
	for _, name := range strings.Split(path[3:], "\\") {
		if !filepath.IsLocal(name) || strings.ContainsAny(name, ":/") || strings.HasSuffix(name, ".") || strings.HasSuffix(name, " ") {
			return false
		}
	}
	return true
}

type profilePins struct {
	root  string
	files []*os.File
}

func (p *profilePins) close() {
	for _, f := range p.files {
		f.Close()
	}
}

func pinConfiguredProfile(path string) (*profilePins, error) {
	p := &profilePins{}
	if err := p.walk(path, true); err != nil {
		p.close()
		return nil, err
	}
	return p, nil
}

// Open every component without following it. Denying write and delete sharing
// on each retained directory prevents retagging or replacing a checked prefix.
func (p *profilePins) walk(path string, allowJunction bool) error {
	if !plainProfilePath(path) {
		return errProfileScope
	}
	root := path[:3]
	drive, err := windows.UTF16PtrFromString(root)
	if err != nil || windows.GetDriveType(drive) != windows.DRIVE_FIXED {
		return errProfileScope
	}
	var fs [32]uint16
	if err := windows.GetVolumeInformation(drive, nil, 0, nil, nil, nil, &fs[0], uint32(len(fs))); err != nil || windows.UTF16ToString(fs[:]) != "NTFS" {
		return errProfileScope
	}
	parts := strings.Split(path[3:], "\\")
	found := false
	cursor := root
	for i, name := range append([]string{""}, parts...) {
		cursor = filepath.Join(cursor, name)
		file, err := openProfileDirectory(cursor, windows.FILE_SHARE_READ)
		if err != nil {
			return err
		}
		p.files = append(p.files, file)
		h := windows.Handle(file.Fd())
		var info windows.ByHandleFileInformation
		if err := windows.GetFileInformationByHandle(h, &info); err != nil {
			return err
		}
		if info.FileAttributes&windows.FILE_ATTRIBUTE_DIRECTORY == 0 {
			return errProfileScope
		}
		if info.FileAttributes&windows.FILE_ATTRIBUTE_REPARSE_POINT != 0 {
			if !allowJunction || found || i <= 1 || i == len(parts) {
				return errProfileScope
			}
			if err := verifyProfileDirectory(windows.Handle(p.files[len(p.files)-2].Fd()), true); err != nil {
				return err
			}
			if err := verifyProfileDirectory(h, true); err != nil {
				return err
			}
			// Inspect the native substitute name. A printable reparse name is
			// advisory and may describe a different destination.
			target, err := junctionSubstitute(h)
			if err != nil {
				return err
			}
			if err := p.walk(target, false); err != nil {
				return err
			}
			cursor = p.root
			found = true
		} else if err := verifyProfileDirectory(h, i == len(parts)); err != nil {
			return err
		}
	}
	if allowJunction && !found {
		return errProfileScope
	}
	canonical, err := canonicalSessionDirectory(cursor)
	if err != nil || !samePath(canonical, cursor) {
		return errProfileScope
	}
	p.root = windowsDOSPath(canonical)
	return nil
}

func openProfileDirectory(path string, share uint32) (*os.File, error) {
	ptr, err := windows.UTF16PtrFromString(path)
	if err != nil {
		return nil, err
	}
	h, err := windows.CreateFile(ptr, windows.FILE_LIST_DIRECTORY|windows.FILE_READ_ATTRIBUTES|windows.READ_CONTROL, share, nil,
		windows.OPEN_EXISTING, windows.FILE_FLAG_BACKUP_SEMANTICS|windows.FILE_FLAG_OPEN_REPARSE_POINT, 0)
	if err != nil {
		return nil, err
	}
	return os.NewFile(uintptr(h), path), nil
}

func junctionSubstitute(h windows.Handle) (string, error) {
	var buf [16384]byte
	var n uint32
	err := windows.DeviceIoControl(h, windows.FSCTL_GET_REPARSE_POINT, nil, 0, &buf[0], uint32(len(buf)), &n, nil)
	if err != nil {
		return "", err
	}
	if n < 16 || n > uint32(len(buf)) || binary.LittleEndian.Uint32(buf[:4]) != windows.IO_REPARSE_TAG_MOUNT_POINT ||
		int(binary.LittleEndian.Uint16(buf[4:6]))+8 != int(n) {
		return "", errProfileScope
	}
	off := int(binary.LittleEndian.Uint16(buf[8:10]))
	size := int(binary.LittleEndian.Uint16(buf[10:12]))
	if off%2 != 0 || size < 2 || size%2 != 0 || 16+off+size > int(n) {
		return "", errProfileScope
	}
	units := make([]uint16, size/2)
	for i := range units {
		units[i] = binary.LittleEndian.Uint16(buf[16+off+i*2:])
		if units[i] == 0 {
			return "", errProfileScope
		}
	}
	decoded := utf16.Decode(units)
	if !slices.Equal(utf16.Encode(decoded), units) {
		return "", errProfileScope
	}
	target, ok := strings.CutPrefix(string(decoded), `\??\`)
	if !ok || !plainProfilePath(target) {
		return "", errProfileScope
	}
	return target, nil
}

// Restrict the alias, its parent, and the destination to the current owner.
// Only allow entries for that user, SYSTEM, or administrators qualify.
func verifyProfileDirectory(h windows.Handle, private bool) error {
	user, err := windows.GetCurrentProcessToken().GetTokenUser()
	if err != nil {
		return err
	}
	sd, err := windows.GetSecurityInfo(h, windows.SE_FILE_OBJECT, windows.OWNER_SECURITY_INFORMATION|windows.DACL_SECURITY_INFORMATION)
	if err != nil {
		return err
	}
	owner, _, err := sd.Owner()
	if err != nil || owner == nil {
		return errProfileScope
	}
	trusted := func(sid *windows.SID) bool {
		return sid.Equals(user.User.Sid) || sid.IsWellKnown(windows.WinLocalSystemSid) || sid.IsWellKnown(windows.WinBuiltinAdministratorsSid)
	}
	// Windows Resource Protection owns some system directories.
	const trustedInstaller = "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464"
	if private && !owner.Equals(user.User.Sid) || !trusted(owner) && owner.String() != trustedInstaller {
		return errProfileScope
	}
	acl, _, err := sd.DACL()
	if err != nil || acl == nil {
		return errProfileScope
	}
	for i := uint16(0); i < acl.AceCount; i++ {
		var ace *windows.ACCESS_ALLOWED_ACE
		if err := windows.GetAce(acl, uint32(i), &ace); err != nil {
			return err
		}
		const inheritOnlyACE = 0x08
		if ace.Header.AceType == windows.ACCESS_DENIED_ACE_TYPE || !private && ace.Header.AceFlags&inheritOnlyACE != 0 {
			continue
		}
		if ace.Header.AceType != windows.ACCESS_ALLOWED_ACE_TYPE {
			return errProfileScope
		}
		sid := (*windows.SID)(unsafe.Pointer(&ace.SidStart))
		// FILE_APPEND_DATA permits adding an unrelated subdirectory.
		const safePrefixRights = windows.FILE_GENERIC_READ | windows.FILE_GENERIC_EXECUTE | windows.GENERIC_READ | windows.GENERIC_EXECUTE | windows.FILE_APPEND_DATA
		if !trusted(sid) && (private || sid.String() != trustedInstaller && ace.Mask&^windows.ACCESS_MASK(safePrefixRights) != 0) {
			return errProfileScope
		}
	}
	return nil
}
