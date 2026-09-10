#include <jni.h>
#include <stdlib.h>
extern char *p2pStart(char *);
extern char *p2pStatus(void);
extern char *p2pProbe(void);
extern char *p2pLogin(void);
extern char *p2pProxy(void);
static jstring take(JNIEnv *env, char *text) {
  jstring result = (*env)->NewStringUTF(env, text);
  free(text);
  return result;
}
JNIEXPORT jstring JNICALL Java_io_neigecalm_next_NativeP2P_start(JNIEnv *env, jobject self, jstring path) {
  const char *dir = (*env)->GetStringUTFChars(env, path, 0);
  char *result = p2pStart((char *)dir);
  (*env)->ReleaseStringUTFChars(env, path, dir);
  return take(env, result);
}
JNIEXPORT jstring JNICALL Java_io_neigecalm_next_NativeP2P_status(JNIEnv *env, jobject self) { return take(env, p2pStatus()); }
JNIEXPORT jstring JNICALL Java_io_neigecalm_next_NativeP2P_probe(JNIEnv *env, jobject self) { return take(env, p2pProbe()); }
JNIEXPORT jstring JNICALL Java_io_neigecalm_next_NativeP2P_proxy(JNIEnv *env, jobject self) { return take(env, p2pProxy()); }

JNIEXPORT jstring JNICALL Java_io_neigecalm_next_NativeP2P_login(JNIEnv *env, jobject self) { return take(env, p2pLogin()); }
