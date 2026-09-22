/** Signed geometry only: the sign does not imply a favorable/unfavorable result. */
export function signedBarLayout(values: readonly number[]) {
  const signed = values.some((value) => value < 0);
  const extent = Math.max(0, ...values.map(Math.abs));
  const zero = signed ? 50 : 0;
  return values.map((value) => {
    const width = extent === 0 ? 0 : Math.abs(value) / extent * (signed ? 50 : 100);
    return { start: value < 0 ? zero - width : zero, width, zero };
  });
}
