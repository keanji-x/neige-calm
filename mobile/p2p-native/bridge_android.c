#include <jni.h>
#include <stdlib.h>
extern char *p2pStart(char *);
extern char *p2pConfigure(char *);
extern char *p2pStatus(void);
extern char *p2pCheck(char *);
extern char *p2pDirect(char *, char *);
extern char *p2pCheckDirect(char *, char *, char *, int);
extern void p2pStopDirect(void);
extern char *p2pEnroll(char *, char *);
extern char *p2pTailnet(char *);
extern char *p2pCancelEnrollment(void);
extern char *p2pResetEnrollment(char *);
extern char *p2pReserveOperation(void);
extern char *p2pCancelOperation(char *);
extern char *p2pConfirmLegacy(char *, char *);
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


JNIEXPORT jstring JNICALL Java_io_neigecalm_next_NativeP2P_direct(JNIEnv *env, jobject self, jstring origin, jstring binding) {
  const char *raw = (*env)->GetStringUTFChars(env, origin, 0);
  const char *saved = (*env)->GetStringUTFChars(env, binding, 0);
  char *result = p2pDirect((char *)raw, (char *)saved);
  (*env)->ReleaseStringUTFChars(env, binding, saved);
  (*env)->ReleaseStringUTFChars(env, origin, raw);
  return take(env, result);
}
JNIEXPORT jstring JNICALL Java_io_neigecalm_next_NativeP2P_checkDirect(JNIEnv *env, jobject self, jstring token, jstring origin, jstring binding, jboolean confirm) {
 const char *admission = (*env)->GetStringUTFChars(env, token, 0);
 const char *raw = (*env)->GetStringUTFChars(env, origin, 0);
 const char *saved = (*env)->GetStringUTFChars(env, binding, 0);
 char *result = p2pCheckDirect((char *)admission, (char *)raw, (char *)saved, confirm ? 1 : 0);
 (*env)->ReleaseStringUTFChars(env, binding, saved);
 (*env)->ReleaseStringUTFChars(env, origin, raw);
 (*env)->ReleaseStringUTFChars(env, token, admission);
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

JNIEXPORT jstring JNICALL Java_io_neigecalm_next_NativeP2P_enroll(JNIEnv *env, jobject self, jstring token, jstring payload) {
 const char *admission = (*env)->GetStringUTFChars(env, token, 0);
 const char *raw = (*env)->GetStringUTFChars(env, payload, 0);
 char *result = p2pEnroll((char *)admission, (char *)raw);
 (*env)->ReleaseStringUTFChars(env, payload, raw);
 (*env)->ReleaseStringUTFChars(env, token, admission);
 return take(env, result);
}

JNIEXPORT jstring JNICALL Java_io_neigecalm_next_NativeP2P_cancelEnrollment(JNIEnv *env, jobject self) { return take(env, p2pCancelEnrollment()); }

JNIEXPORT jstring JNICALL Java_io_neigecalm_next_NativeP2P_resetEnrollment(JNIEnv *env, jobject self, jstring token) {
 const char *admission = (*env)->GetStringUTFChars(env, token, 0);
 char *result = p2pResetEnrollment((char *)admission);
 (*env)->ReleaseStringUTFChars(env, token, admission);
 return take(env, result);
}
JNIEXPORT jstring JNICALL Java_io_neigecalm_next_NativeP2P_reserveOperation(JNIEnv *env, jobject self) { return take(env, p2pReserveOperation()); }
JNIEXPORT jstring JNICALL Java_io_neigecalm_next_NativeP2P_cancelOperation(JNIEnv *env, jobject self, jstring token) {
 const char *admission = (*env)->GetStringUTFChars(env, token, 0);
 char *result = p2pCancelOperation((char *)admission);
 (*env)->ReleaseStringUTFChars(env, token, admission);
 return take(env, result);
}
JNIEXPORT jstring JNICALL Java_io_neigecalm_next_NativeP2P_confirmLegacy(JNIEnv *env, jobject self, jstring token, jstring origin) {
 const char *admission = (*env)->GetStringUTFChars(env, token, 0);
 const char *target = (*env)->GetStringUTFChars(env, origin, 0);
 char *result = p2pConfirmLegacy((char *)admission, (char *)target);
 (*env)->ReleaseStringUTFChars(env, origin, target);
 (*env)->ReleaseStringUTFChars(env, token, admission);
 return take(env, result);
}
