import { build } from 'esbuild';
await build({entryPoints:['src/editor.ts'],bundle:true,format:'iife',target:'safari16',outfile:'dist/editor.js',minify:true,legalComments:'eof'});
