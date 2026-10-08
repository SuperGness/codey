const DEFAULT_MAX_BYTES = 48 * 1024;
const NOTE_CATEGORIES = ["新增功能", "体验优化", "问题修复"];
const DEFAULT_NOTE_CATEGORY = "体验优化";

function noteCategory(entry) {
  const category = entry.category ?? DEFAULT_NOTE_CATEGORY;
  if (!NOTE_CATEGORIES.includes(category)) throw new Error("日志分类必须为新增功能、体验优化或问题修复");
  return category;
}

export function formatNoteResults(candidates) {
  const evidence = NOTE_CATEGORIES.flatMap(category => candidates.filter(candidate => noteCategory(candidate) === category));
  const sections = NOTE_CATEGORIES.flatMap(category => {
    const entries = evidence.filter(candidate => noteCategory(candidate) === category);
    return entries.length ? [`**${category}**\n${entries.map(entry => `- ${entry.note}`).join("\n")}`] : [];
  });
  return { notes: sections.join("\n\n"), evidence };
}

export function parseNoteLines(notes) {
  const lines = notes.trim().split(/\r?\n/).filter(line => line.trim());
  const grouped = lines.some(line => /^\*\*/.test(line));
  const entries = [];
  let category;
  let categoryIndex = -1;
  let sectionHasNotes = false;
  for (const line of lines) {
    const heading = /^\*\*(新增功能|体验优化|问题修复)\*\*$/.exec(line);
    if (heading) {
      const nextIndex = NOTE_CATEGORIES.indexOf(heading[1]);
      if (nextIndex <= categoryIndex || (category && !sectionHasNotes)) throw new Error("日志分类必须按固定顺序排列且包含内容");
      category = heading[1];
      categoryIndex = nextIndex;
      sectionHasNotes = false;
    } else {
      if (!line.startsWith("- ") || (grouped && !category)) throw new Error("每条日志必须使用列表格式并对应一条差异证据");
      entries.push({ note: line.slice(2), category });
      sectionHasNotes = true;
    }
  }
  if (category && !sectionHasNotes) throw new Error("日志分类必须包含内容");
  return entries;
}

function serializedBytes(value) {
  return Buffer.byteLength(JSON.stringify(value), "utf8");
}

function splitDiffLines(diff) {
  if (diff.length === 0) return [""];
  const lines = [];
  let start = 0;
  while (start < diff.length) {
    const end = diff.indexOf("\n", start);
    if (end < 0) {
      lines.push(diff.slice(start));
      break;
    }
    lines.push(diff.slice(start, end + 1));
    start = end + 1;
  }
  return lines;
}

function validateMaxBytes(maxBytes) {
  if (!Number.isSafeInteger(maxBytes) || maxBytes < 1) throw new Error("单批大小必须是正整数");
}

export function chunkPatches(patches, maxBytes = DEFAULT_MAX_BYTES, measure = serializedBytes) {
  validateMaxBytes(maxBytes);
  if (!Array.isArray(patches)) throw new TypeError("差异补丁必须是数组");
  if (typeof measure !== "function") throw new TypeError("差异大小计算方式无效");

  const chunks = [];
  for (const patch of patches) {
    if (!patch || typeof patch.file !== "string" || typeof patch.diff !== "string") throw new TypeError("差异补丁必须包含文件路径和文本差异");
    let fragment = "";
    for (const line of splitDiffLines(patch.diff)) {
      const candidate = fragment + line;
      const entry = { file: patch.file, diff: candidate };
      if (measure([entry]) > maxBytes) {
        if (!fragment) throw new Error(`单行差异与文件路径序列化后超过单批大小限制：${patch.file}`);
        chunks.push({ file: patch.file, diff: fragment });
        fragment = line;
        if (measure([{ file: patch.file, diff: fragment }]) > maxBytes) throw new Error(`单行差异与文件路径序列化后超过单批大小限制：${patch.file}`);
      } else {
        fragment = candidate;
      }
    }
    chunks.push({ file: patch.file, diff: fragment });
  }

  const batches = [];
  let batch = [];
  for (const chunk of chunks) {
    const candidate = [...batch, chunk];
    if (measure(candidate) > maxBytes) {
      if (batch.length) batches.push(batch);
      batch = [chunk];
    } else {
      batch = candidate;
    }
  }
  if (batch.length) batches.push(batch);
  return batches;
}

function excerptForLine(lines, index) {
  let start = index;
  let end = index + 1;
  let excerpt = lines[index].replace(/\n$/, "");
  while (excerpt.length < 8 && (start > 0 || end < lines.length)) {
    if (start > 0) start -= 1;
    else end += 1;
    excerpt = lines.slice(start, end).join("");
  }
  return excerpt.length >= 8 ? excerpt : null;
}

export function createNoteBatchInput(patches, batchIndex = 0) {
  const references = new Map();
  const annotated = patches.map((patch, patchIndex) => {
    const lines = splitDiffLines(patch.diff);
    const diff = lines.map((line, lineIndex) => {
      if (!/^[+-](?![+-]{2}).+/.test(line)) return line;
      const excerpt = excerptForLine(lines, lineIndex);
      if (!excerpt) return line;
      const ref = `b${String(batchIndex + 1).padStart(10, "0")}p${patchIndex}l${lineIndex}`;
      references.set(ref, { file: patch.file, excerpt });
      return `[evidence:${ref}] ${line}`;
    }).join("");
    return { file: patch.file, diff };
  });
  return { patches: annotated, references };
}

export function chunkNotePatches(patches, maxBytes = DEFAULT_MAX_BYTES) {
  return chunkPatches(patches, maxBytes, batch => serializedBytes(createNoteBatchInput(batch).patches));
}

export function resolveNoteEntries(value, references) {
  if (!value || !Array.isArray(value.entries)) throw new Error("AI 输出必须包含 entries 数组");
  const evidence = value.entries.map((entry, index) => {
    if (!entry || typeof entry.note !== "string" || !entry.note.trim() || /[\r\n]/.test(entry.note)) throw new Error(`第 ${index + 1} 条日志文字无效`);
    const reference = typeof entry.ref === "string" ? references.get(entry.ref) : null;
    if (!reference) throw new Error(`第 ${index + 1} 条日志引用的变更编号不属于当前批次`);
    return { note: entry.note.trim(), category: noteCategory(entry), ...reference };
  });
  return formatNoteResults(evidence);
}

export function collectNoteCandidates(results) {
  if (!Array.isArray(results)) throw new TypeError("日志结果必须是数组");
  const candidates = [];
  for (const result of results) {
    if (!result || typeof result.notes !== "string" || !Array.isArray(result.evidence)) throw new Error("分批日志结果无效");
    if (result.notes === "" && result.evidence.length === 0) continue;
    const lines = parseNoteLines(result.notes);
    if (lines.length !== result.evidence.length) throw new Error("分批日志与差异证据数量不一致");
    for (const [index, line] of lines.entries()) {
      if (typeof result.evidence[index]?.note !== "string" || result.evidence[index].note !== line.note || (line.category && line.category !== noteCategory(result.evidence[index]))) throw new Error("分批日志与差异证据不一致");
      candidates.push(result.evidence[index]);
    }
  }
  return candidates;
}

export function mergeNoteResults(results) {
  const candidates = collectNoteCandidates(results);
  const evidence = [];
  const seen = new Set();
  for (const candidate of candidates) {
    const note = candidate.note;
    if (seen.has(note)) continue;
    seen.add(note);
    evidence.push(candidate);
  }
  return formatNoteResults(evidence);
}
