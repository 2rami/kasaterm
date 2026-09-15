import { test } from 'node:test';
import assert from 'node:assert/strict';
import { floatingPlacement } from '../src/floating.ts';
const bounds = {left:252,top:8,right:992,bottom:712};
test('selection toolbar stays inside document canvas rather than covering the vault sidebar',()=>{
  const result=floatingPlacement({left:265,right:280,top:220,bottom:240},360,44,bounds,'above');
  assert.equal(result.left,252); assert.equal(result.top,168); assert.equal(result.side,'above');
});
test('slash menu flips above a caret near the viewport bottom',()=>{
  const result=floatingPlacement({left:500,right:500,top:650,bottom:670},272,360,bounds,'below');
  assert.equal(result.side,'above'); assert.ok(result.top+360<650); assert.ok(result.top>=bounds.top);
});
test('toolbar flips below a top-edge selection and wraps in a narrow viewport',()=>{
  const result=floatingPlacement({left:16,right:70,top:10,bottom:30},360,76,{left:8,top:8,right:312,bottom:500},'above');
  assert.equal(result.side,'below'); assert.equal(result.top,38); assert.equal(result.maxWidth,304); assert.equal(result.left,8);
});
test('tall menu uses the available side height instead of overlapping the caret',()=>{
  const result=floatingPlacement({left:80,right:80,top:170,bottom:190},272,380,{left:8,top:8,right:312,bottom:350},'below');
  assert.equal(result.side,'above'); assert.equal(result.maxHeight,154); assert.equal(result.top,8);
});
