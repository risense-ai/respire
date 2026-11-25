package host

import "respire-client2/internal/i18n"

import (
	"encoding/json"
	"errors"
	"fmt"
	"math"
	"strconv"
	"strings"
)

type argument struct {
	key, flag, kind string
	required        bool
	choices         []string
	max             float64
}

type commandSpec struct {
	words       []string
	args        []argument
	method      string
	host, write bool
}

func pos(key string) argument       { return argument{key: key, kind: "string", required: true} }
func opt(key, flag string) argument { return argument{key: key, flag: flag, kind: "string"} }
func requiredOpt(key, flag string) argument {
	return argument{key: key, flag: flag, kind: "string", required: true}
}
func flag(key, name string) argument { return argument{key: key, flag: name, kind: "flag"} }
func integer(key, name string, max float64) argument {
	return argument{key: key, flag: name, kind: "int", max: max}
}
func choice(key, name string, choices ...string) argument {
	return argument{key: key, flag: name, kind: "string", choices: choices}
}

func (spec commandSpec) build(raw json.RawMessage) ([]string, error) {
	input := map[string]any{}
	if len(raw) > 0 {
		if err := json.Unmarshal(raw, &input); err != nil {
			return nil, err
		}
		if input == nil {
			return nil, errors.New(i18n.Messagefe1145505fdf06cb)
		}
	}
	var options, positions []string
	for _, p := range spec.args {
		value, exists := input[p.key]
		delete(input, p.key)
		if !exists || value == nil {
			if p.required {
				return nil, fmt.Errorf(i18n.Message240b7606fd5f08b1, p.key)
			}
			continue
		}
		var text string
		switch p.kind {
		case "string", "json":
			var ok bool
			text, ok = value.(string)
			if !ok || strings.ContainsRune(text, 0) {
				return nil, fmt.Errorf(i18n.Messagebd4db12886f474b2, p.key)
			}
			if p.required && strings.TrimSpace(text) == "" {
				return nil, fmt.Errorf(i18n.Message0833fa7e8b77400c, p.key)
			}
			if p.kind == "json" && !json.Valid([]byte(text)) {
				return nil, fmt.Errorf(i18n.Message1d7ae2a999915095, p.key)
			}
			if len(p.choices) > 0 {
				found := false
				for _, c := range p.choices {
					found = found || text == c
				}
				if !found {
					return nil, fmt.Errorf(i18n.Message262202b6a0bffc42, p.key)
				}
			}
		case "int", "ratio":
			n, ok := value.(float64)
			if !ok || math.IsNaN(n) || math.IsInf(n, 0) || n < 0 || n > p.max || p.kind == "int" && n != math.Trunc(n) {
				return nil, fmt.Errorf(i18n.Message914e9bac37e746d1, p.key)
			}
			text = strconv.FormatFloat(n, 'f', -1, 64)
		case "flag", "bool":
			b, ok := value.(bool)
			if !ok {
				return nil, fmt.Errorf(i18n.Messagedbee957669291a03, p.key)
			}
			if p.kind == "flag" {
				if b {
					options = append(options, p.flag)
				}
				continue
			}
			text = strconv.FormatBool(b)
		default:
			return nil, errors.New(i18n.Message9669ea1d64b8ca63)
		}
		if p.flag == "" {
			positions = append(positions, text)
		} else {
			options = append(options, p.flag+"="+text)
		}
	}
	for key := range input {
		return nil, fmt.Errorf(i18n.Messagee6309dfdb198c141, key)
	}
	args := append(append([]string{}, spec.words...), options...)
	if len(positions) > 0 {
		if spec.method == "" {
			args = append(args, "--")
		}
		args = append(args, positions...)
	}
	return args, nil
}

func operationSpec(name string) (commandSpec, bool) {
	for _, group := range []map[string]commandSpec{memoryCommands, runtimeCommands, settingsCommands} {
		if spec, ok := group[name]; ok {
			return spec, true
		}
	}
	return commandSpec{}, false
}
