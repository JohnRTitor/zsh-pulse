#include <stdlib.h>
#include <stdio.h>
typedef char *Options; typedef void *Module;
typedef int (*HandlerFunc)(char *, char **, Options, int);
typedef struct hashnode { struct hashnode *next; char *nam; int flags; } HashNode;
typedef struct builtin { HashNode node; HandlerFunc handlerfunc; int minargs, maxargs, funcid; char *optstr, *defopts; } Builtin;
extern void addbuiltins(char const *, Builtin *, int);
extern char *getsparam(char *);           /* CORRECT: returns char * */
static int h5(char *n,char **a,Options o,int l){(void)n;(void)a;(void)o;(void)l;
  char *buf = getsparam((char*)"BUFFER");
  char *cur = getsparam((char*)"CURSOR");
  fprintf(stderr,"pulse BUFFER=[%s] CURSOR=[%s]\n", buf?buf:"(null)", cur?cur:"(null)");
  return 0;}
static Builtin bl[] = { { { NULL,(char*)"p5-hl",0 }, h5, 0,-1,0,NULL,NULL }, { {NULL,NULL,0},NULL,0,0,0,NULL,NULL } };
int setup_(Module m){(void)m; addbuiltins((char*)"p5-hl", bl, 1); return 0;}
int boot_(Module m){(void)m;return 0;} int cleanup_(Module m){(void)m;return 0;} int finish_(Module m){(void)m;return 0;}
