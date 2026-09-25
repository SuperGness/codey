export function grayTargetCount(
  total: number,
  percentage: number | null,
  deviceLimit: number | null,
  whitelistOnly: boolean,
): number {
  if (!Number.isInteger(total) || total < 0) throw new RangeError("total");
  if (whitelistOnly) return total;
  if ((percentage == null) === (deviceLimit == null)) throw new RangeError("percentage or deviceLimit");
  if (percentage != null) return Math.min(total, Math.ceil(total * percentage / 100));
  return Math.min(total, deviceLimit!);
}

export function isIdempotencyKey(value: string): boolean {
  return value.length >= 8 && value.length <= 200 && /^[A-Za-z0-9._:-]+$/.test(value);
}
