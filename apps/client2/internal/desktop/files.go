package desktop

import "respire-client2/internal/i18n"

import "errors"

func (d *desktop) chooseFile(kind string) (string, error) {
	switch kind {
	case "directory":
		return d.app.Dialog.OpenFile().SetTitle(i18n.Message5fe04bd62763d02a).CanChooseDirectories(true).CanChooseFiles(false).AttachToWindow(d.window).PromptForSingleSelection()
	case "import":
		return d.app.Dialog.OpenFile().SetTitle(i18n.Messagebb6b8ef838716c2b).AddFilter(i18n.Message97f698e0b9af9922, "*.json").AttachToWindow(d.window).PromptForSingleSelection()
	case "share-import":
		return d.app.Dialog.OpenFile().SetTitle(i18n.Message34cc9246ad36c83c).AddFilter(i18n.Message66361095a9e74711, "*.txt;*.md").AttachToWindow(d.window).PromptForSingleSelection()
	case "export":
		return d.app.Dialog.SaveFile().SetMessage(i18n.Messaged40520260c4ea876).SetFilename("respire-memories.json").AddFilter(i18n.Message97f698e0b9af9922, "*.json").AttachToWindow(d.window).PromptForSingleSelection()
	case "backup":
		return d.app.Dialog.SaveFile().SetMessage(i18n.Messagee7843629b018f822).SetFilename("respire-backup.db").AddFilter(i18n.Messagea5dd79dec780abca, "*.db").AttachToWindow(d.window).PromptForSingleSelection()
	case "keys":
		return d.app.Dialog.SaveFile().SetMessage(i18n.Messageb0a5f7f526c98c55).SetFilename("respire-keys.txt").AddFilter(i18n.Message5dc1f920f6c8ef19, "*.txt").AttachToWindow(d.window).PromptForSingleSelection()
	case "preferences-export":
		return d.app.Dialog.SaveFile().SetMessage(i18n.Message91354952b391fd98).SetFilename("respire-settings.json").AddFilter(i18n.Message38c6e30e88ed41df, "*.json").AttachToWindow(d.window).PromptForSingleSelection()
	case "preferences-import":
		return d.app.Dialog.OpenFile().SetTitle(i18n.Message56d513d442ba2d4e).AddFilter(i18n.Message38c6e30e88ed41df, "*.json").AttachToWindow(d.window).PromptForSingleSelection()
	case "diagnostics-export":
		return d.app.Dialog.SaveFile().SetMessage(i18n.Messaged2fa28ffbe6597d0).SetFilename("respire-diagnostics.json").AddFilter(i18n.Message6752caab44e8356e, "*.json").AttachToWindow(d.window).PromptForSingleSelection()
	default:
		return "", errors.New(i18n.Message3b89864e99693678)
	}
}
