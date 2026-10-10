// Independent ECMAScript number-format oracle; never imports Mneme.
const fs = require('fs');
const destination = process.argv[2];
let state=0x71eafb934f281ba5n;
const mask=(1n<<64n)-1n;
function next() { state ^= state<<13n; state ^= state>>7n; state ^= state<<17n; state &= mask; return state; }
const fixed=[0n,0x8000000000000000n,1n,0x8000000000000001n,0x7ff0000000000000n,0xfff0000000000000n,0x7ff8000000000000n,0x4340000000000000n,0x433fffffffffffffn,0x3eb0c6f7a0b5ed8dn];
const cases=[];
for(let i=0;i<512;i++) {
  const bits=i<fixed.length?fixed[i]:next();
  const data=Buffer.alloc(8); data.writeBigUInt64BE(bits);
  const value=data.readDoubleBE();
  const expected=!Number.isFinite(value)?'value.number_not_finite':(Number.isInteger(value)&&Math.abs(value)>Number.MAX_SAFE_INTEGER)?'value.integer_out_of_range':JSON.stringify(value);
  const row={bits:bits.toString(16).padStart(16,'0'),expected};
  if(Number.isFinite(value)) row.text=value.toPrecision(24);
  cases.push(row);
}
fs.mkdirSync(require('path').dirname(destination),{recursive:true});
fs.writeFileSync(destination,'[\n'+cases.map(row=>'  '+JSON.stringify(row)).join(',\n')+'\n]\n');
