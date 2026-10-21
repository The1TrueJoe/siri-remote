#include "proto.h"
#include <stdio.h>
#include <string.h>
#include <math.h>
static int fails=0;
#define CHECK(c) do{ if(!(c)){ printf("FAIL: %s\n", #c); fails++; } }while(0)
static size_t unhex(const char*h, uint8_t*o){ size_t n=0; for(;h[0]&&h[1];h+=2){ unsigned v; sscanf(h,"%2x",&v); o[n++]=(uint8_t)v;} return n; }
int main(void){
  uint8_t buf[256]; struct siri_report r;
  /* real A1513 Speex notification (82 bytes, Siri held) */
  const char*SPX="1010377088766f000068e8e8e8e8e8e8e88000347474747474747444801a3a3d7fcf481606815f7d1b46b6b71715964b3865ababababab5ababababab294345075d82abababab6474600854bc2090ad7a33c";
  size_t n=unhex(SPX,buf); CHECK(n==82);
  siri_proto_parse(buf,n,&r);
  CHECK(r.kind==SIRI_REPORT_AUDIO); CHECK(r.codec==SIRI_CODEC_SPEEX);
  CHECK(r.buttons & SIRI_BTN_SIRI); CHECK(r.frame_len==70);
  CHECK(r.stream_time>0.0199f && r.stream_time<0.0201f);
  /* buttons-only: menu down */
  uint8_t b[2]={0x00,SIRI_BTN_MENU}; siri_proto_parse(b,2,&r);
  CHECK(r.kind==SIRI_REPORT_BUTTONS); CHECK(r.buttons==SIRI_BTN_MENU);
  /* audio-end marker */
  uint8_t e[2]={0x10,0x00}; siri_proto_parse(e,2,&r);
  CHECK(r.kind==SIRI_REPORT_AUDIO_END);
  /* one-finger touch (13 bytes): data[2]=0x32 */
  uint8_t t[13]={0x01,0x00,0x32,0,0,0, 230,0,188,0,0,41,0}; siri_proto_parse(t,13,&r);
  CHECK(r.kind==SIRI_REPORT_TOUCH); CHECK(r.n_fingers==1);
  printf(fails? "\n%d CHECK(S) FAILED\n" : "all decoder checks passed\n", fails);
  return fails?1:0;
}
