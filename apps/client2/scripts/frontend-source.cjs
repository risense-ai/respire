const fs = require('node:fs');
const path = require('node:path');

const frontend = path.resolve(__dirname, '../internal/desktop/frontend');
const directory = path.join(frontend, 'ui/i18n');
const i18nMessages = {};
for (const file of fs.readdirSync(directory).filter(file => /^messages\d+\.js$/.test(file))) {
  const source = fs.readFileSync(path.join(directory, file), 'utf8');
  const name = path.basename(file, '.js');
  Object.assign(i18nMessages, new Function(source.replace(/^export /gm, '') + `\nreturn ${name};`)());
}

// Inline only imported constant language data for the source-execution harness.
function readFrontend(file) {
  let content = fs.readFileSync(path.join(frontend, file), 'utf8');
  if (file === 'ui/i18n.js') {
    content = content.replace(/^import \{ (translations\d+) \} from "\.\/i18n\/\1\.js";\r?\n/gm,
      (match, name) => fs.readFileSync(path.join(directory, name + '.js'), 'utf8').replace(/^export /gm, ''));
  }
  const source = content.replace(
    /^import \{ messages as i18nMessages \} from [^\n]+;\r?\n/m, '',
  );
  return source.replace(/\$\{i18nMessages\.(m[0-9a-f]+)\}|i18nMessages\.(m[0-9a-f]+)/g,
    (match, template, literal) => {
      const key = template || literal;
      if (!Object.hasOwn(i18nMessages, key)) throw new Error(`Missing UI message ${key}`);
      const value = JSON.stringify(i18nMessages[key]);
      return template ? value.slice(1, -1).replaceAll('\\"', '"').replaceAll('`', '\\`').replaceAll('${', '\\${') : value;
    });
}

module.exports = { i18nMessages, readFrontend };
