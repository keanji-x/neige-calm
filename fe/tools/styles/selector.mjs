/** @param {string} selector */
export function rightmostCompound(selector) {
  let square = 0;
  let round = 0;
  let boundary = 0;
  for (let index = 0; index < selector.length; index += 1) {
    const char = selector[index];
    if (char === '[') square += 1;
    else if (char === ']') square -= 1;
    else if (char === '(') round += 1;
    else if (char === ')') round -= 1;
    else if (square === 0 && round === 0 && ['>', '+', '~', ' '].includes(char)) boundary = index + 1;
  }
  return selector.slice(boundary).trim();
}

/** @param {string} selector @param {boolean} [includeFunctionalPseudoClasses] */
export function classes(selector, includeFunctionalPseudoClasses = false) {
  const found = [];
  let square = 0;
  let round = 0;
  for (let index = 0; index < selector.length; index += 1) {
    if (selector[index] === '[') { square += 1; continue; }
    if (selector[index] === ']') { square -= 1; continue; }
    if (selector[index] === '(') { round += 1; continue; }
    if (selector[index] === ')') { round -= 1; continue; }
    if (square !== 0 || (!includeFunctionalPseudoClasses && round !== 0)) continue;
    if (selector[index] !== '.') continue;
    let cursor = index + 1;
    let name = '';
    while (cursor < selector.length) {
      const char = selector[cursor];
      const code = char?.charCodeAt(0) ?? 0;
      const valid = char === '-' || char === '_' || (code >= 48 && code <= 57) || (code >= 65 && code <= 90) || (code >= 97 && code <= 122) || code >= 128;
      if (!valid) break;
      name += char;
      cursor += 1;
    }
    if (name) found.push(name);
    index = cursor - 1;
  }
  return found;
}
