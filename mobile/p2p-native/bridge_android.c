#include <jni.h>
#include <stdlib.h>
extern char *p2pStart(char *);
extern char *p2pConfigure(char *);
extern char *p2pStatus(void);
extern char *p2pCheck(char *);
extern char *p2pDirect(char *);
extern void p2pStopDirect(void);
extern char *p2pEnroll(char *);
extern char *p2pTailnet(char *);
extern char *p2pCancelEnrollment(void);
extern char *p2pResetEnrollment(void);
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


JNIEXPORT jstring JNICALL Java_io_neigecalm_next_NativeP2P_configure(JNIEnv *env, jobject self, jstring snapshot) {
  const char *raw = (*env)->GetStringUTFChars(env, snapshot, 0);
  char *result = p2pConfigure((char *)raw);
  (*env)->ReleaseStringUTFChars(env, snapshot, raw);
  return take(env, result);
}


JNIEXPORT jstring JNICALL Java_io_neigecalm_next_NativeP2P_direct(JNIEnv *env, jobject self, jstring origin) {
  const char *raw = (*env)->GetStringUTFChars(env, origin, 0);
  char *result = p2pDirect((char *)raw);
  (*env)->ReleaseStringUTFChars(env, origin, raw);
  return take(env, result);
}
JNIEXPORT void JNICALL Java_io_neigecalm_next_NativeP2P_stopDirect(JNIEnv *env, jobject self) { p2pStopDirect(); }

JNIEXPORT jstring JNICALL Java_io_neigecalm_next_NativeP2P_check(JNIEnv *env, jobject self, jstring origin) {
 const char *raw = (*env)->GetStringUTFChars(env, origin, 0);
 char *result = p2pCheck((char *)raw);
 (*env)->ReleaseStringUTFChars(env, origin, raw);
 return take(env, result);
}

JNIEXPORT jstring JNICALL Java_io_neigecalm_next_NativeP2P_tailnet(JNIEnv *env, jobject self, jstring origin) {
 const char *raw = (*env)->GetStringUTFChars(env, origin, 0);
 char *result = p2pTailnet((char *)raw);
 (*env)->ReleaseStringUTFChars(env, origin, raw);
 return take(env, result);
}

JNIEXPORT jstring JNICALL Java_io_neigecalm_next_NativeP2P_enroll(JNIEnv *env, jobject self, jstring payload) {
 const char *raw = (*env)->GetStringUTFChars(env, payload, 0);
 char *result = p2pEnroll((char *)raw);
 (*env)->ReleaseStringUTFChars(env, payload, raw);
 return take(env, result);
}

JNIEXPORT jstring JNICALL Java_io_neigecalm_next_NativeP2P_cancelEnrollment(JNIEnv *env, jobject self) { return take(env, p2pCancelEnrollment()); }

JNIEXPORT jstring JNICALL Java_io_neigecalm_next_NativeP2P_resetEnrollment(JNIEnv *env, jobject self) { return take(env, p2pResetEnrollment()); }
