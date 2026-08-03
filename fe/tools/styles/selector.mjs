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
