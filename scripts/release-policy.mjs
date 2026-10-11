const versionPattern = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-((?:0|[1-9]\d*|[0-9A-Za-z-]*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9]\d*|[0-9A-Za-z-]*[A-Za-z-][0-9A-Za-z-]*))*))?$/;

export function validateReleaseVersion(version) {
  const match = typeof version === "string" && version.length <= 128 ? version.match(versionPattern) : null;
  if (!match || match[0] !== version || !match.slice(1, 4).every(part => Number.isSafeInteger(Number(part)))) throw new Error("发布版本号必须为有效的 SemVer，不支持 v 前缀或构建元数据");
  return version;
}

export function releaseIdentity(version) {
  validateReleaseVersion(version);
  const prerelease = version.includes("-");
  const channel = prerelease ? "experimental" : "stable";
  const tag = `v${version}`;
  return { version, tag, channel, prerelease, name: `${prerelease ? "实验版" : "正式版"} ${tag}` };
}

export function resolveReleaseVersion(version, channel) {
  const identity = releaseIdentity(version);
  if (channel === undefined) return identity;
  if (!["stable", "experimental"].includes(channel)) throw new Error("发布类型无效");
  if (channel === "stable" && identity.prerelease) throw new Error("正式版不能包含测试后缀");
  return releaseIdentity(channel === "experimental" && !identity.prerelease ? `${version}-beta.1` : version);
}

export function compareReleaseVersions(left, right) {
  validateReleaseVersion(left);
  validateReleaseVersion(right);
  const [leftCore, leftPre] = left.split("-");
  const [rightCore, rightPre] = right.split("-");
  const leftParts = leftCore.split(".").map(Number);
  const rightParts = rightCore.split(".").map(Number);
  for (let index = 0; index < 3; index++) {
    if (leftParts[index] !== rightParts[index]) return leftParts[index] < rightParts[index] ? -1 : 1;
  }
  if (leftPre === undefined || rightPre === undefined) return leftPre === rightPre ? 0 : leftPre === undefined ? 1 : -1;
  const leftIdentifiers = left.slice(leftCore.length + 1).split(".");
  const rightIdentifiers = right.slice(rightCore.length + 1).split(".");
  for (let index = 0; index < Math.max(leftIdentifiers.length, rightIdentifiers.length); index++) {
    const leftId = leftIdentifiers[index], rightId = rightIdentifiers[index];
    if (leftId === rightId) continue;
    if (leftId === undefined || rightId === undefined) return leftId === undefined ? -1 : 1;
    const leftNumeric = /^\d+$/.test(leftId), rightNumeric = /^\d+$/.test(rightId);
    if (leftNumeric && rightNumeric) return BigInt(leftId) < BigInt(rightId) ? -1 : 1;
    if (leftNumeric !== rightNumeric) return leftNumeric ? -1 : 1;
    return leftId < rightId ? -1 : 1;
  }
  return 0;
}
