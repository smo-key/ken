// Syntax highlighting for the Files code viewer. A file's language comes from
// its name (CodeMirror's language list, already in the app through the
// Markdown editor's code blocks), its parse from Lezer, and its colours from
// the app's tokens (`--syntax-*` in app.css), so light and dark follow the
// theme. Each grammar loads on first use, as its own chunk.
import { LanguageDescription, type Language } from "@codemirror/language";
import { languages } from "@codemirror/language-data";
import { classHighlighter, highlightCode } from "@lezer/highlight";

export type Parser = Language["parser"];

/** Past this many characters a file is left plain: parsing and painting it
 *  again on every keystroke would cost more than the colour is worth. */
export const HIGHLIGHT_MAX = 100_000;

/** The parser for the language a file's name says, loaded on first use;
 *  null when no language is known for it or its grammar fails to load. */
export async function parserFor(path: string): Promise<Parser | null> {
  const name = path.split(/[\\/]/).pop() ?? path;
  const desc = LanguageDescription.matchFilename(languages, name);
  if (!desc) return null;
  try {
    const support = desc.support ?? (await desc.load());
    return support.language.parser;
  } catch {
    return null;
  }
}

/** Text for HTML. A carriage return is dropped: the textarea the HTML sits
 *  behind shows "\r\n" as one line break, and the HTML must lay out the same. */
function escapeText(text: string): string {
  return text.replace(/\r/g, "").replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

/** `code` as HTML: each token in a span of its `tok-*` classes
 *  (`classHighlighter`), the rest as escaped text, and line breaks kept as
 *  "\n", so it lays out exactly as the text does in a `pre-wrap` box. Null
 *  past `HIGHLIGHT_MAX`. */
export function highlightHtml(code: string, parser: Parser): string | null {
  if (code.length > HIGHLIGHT_MAX) return null;
  let out = "";
  highlightCode(
    code,
    parser.parse(code),
    classHighlighter,
    (text, classes) => {
      out += classes ? `<span class="${classes}">${escapeText(text)}</span>` : escapeText(text);
    },
    () => {
      out += "\n";
    },
  );
  // A last empty line still takes its height, as it does in the textarea.
  if (out.endsWith("\n")) out += " ";
  return out;
}
