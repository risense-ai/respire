

function same(left, right) {
  if (left && right && typeof left === "object" && typeof right === "object") {
    if (Array.isArray(left) !== Array.isArray(right)) return false;
    const keys = Object.keys(left).sort();
    return keys.join(",") === Object.keys(right).sort().join(",") &&
      keys.every(key => same(left[key], right[key]));
  }
  return left === right;
}

export { same };
