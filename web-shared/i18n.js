// The pages in the reader's language.
//
// Two languages, chosen from the browser and overridable with `?lang=`. The
// dictionaries live beside the page that uses them; this file is only the
// mechanism, which is small enough not to need a library.
//
// Static text is marked in the HTML with `data-i18n="key"`. The English is
// written in the HTML itself, so a page whose script failed to load still
// reads as a page.

const requested = new URLSearchParams(location.search).get('lang');
const preferred = (requested ?? navigator.language ?? 'en').toLowerCase();

/** The language in use: `pt` or `en`. */
export const language = preferred.startsWith('pt') ? 'pt' : 'en';

/**
 * Makes a translator from a dictionary of the form
 * `{ key: { en: '…', pt: '…' } }`. Placeholders are written `{name}`.
 */
export function translator(dictionary) {
  return function translate(key, values = {}) {
    const entry = dictionary[key];
    const template = entry?.[language] ?? entry?.en ?? key;
    return template.replace(/\{(\w+)\}/g, (whole, name) =>
      name in values ? String(values[name]) : whole,
    );
  };
}

/** Replaces the text of every element marked `data-i18n`. */
export function translatePage(translate) {
  document.documentElement.lang = language === 'pt' ? 'pt-BR' : 'en';
  for (const node of document.querySelectorAll('[data-i18n]')) {
    node.textContent = translate(node.dataset.i18n);
  }
  const title = document.querySelector('title[data-i18n-title]');
  if (title) document.title = translate(title.dataset.i18nTitle);
}
