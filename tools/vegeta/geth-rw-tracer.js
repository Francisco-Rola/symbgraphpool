{
  reads: {},
  writes: {},
  steps: 0,
  byte2Hex: function(byte) {
    if (byte < 0x10) { return "0" + byte.toString(16); }
    return byte.toString(16);
  },
  array2Hex: function(arr) {
    var out = "";
    for (var i = 0; i < arr.length; i++) { out += this.byte2Hex(arr[i]); }
    return out;
  },
  pad64: function(value) {
    while (value.length < 64) { value = "0" + value; }
    return value;
  },
  storageKey: function(log) {
    return "evm/" + this.array2Hex(log.contract.getAddress()).toLowerCase() + "/" +
      this.pad64(log.stack.peek(0).toString(16).toLowerCase());
  },
  step: function(log, db) {
    this.steps++;
    var opcode = log.op.toNumber();
    if (opcode == 0x54) {
      this.reads[this.storageKey(log)] = true;
    }
    if (opcode == 0x55) {
      this.writes[this.storageKey(log)] = true;
    }
  },
  fault: function(log, db) {},
  result: function(ctx, db) {
    return {
      reads: Object.keys(this.reads).sort(),
      writes: Object.keys(this.writes).sort(),
      steps: this.steps,
      gasUsed: ctx.gasUsed,
      error: ctx.error || ""
    };
  }
}
