import { useMutation, type UseMutationOptions, type UseMutationResult, type MutationFunctionContext } from '@tanstack/react-query';
import type { ApiTransportPort } from '../../../../core/api/types.ts';

/** Captured at the user's call boundary, before React Query or a serial queue accepts work. */
export function admitTransport(transport: ApiTransportPort): ApiTransportPort {
  if (transport.recovery) return transport.recovery.scope(transport.recovery.capture());
  if (__NC_BUNDLED__) throw new Error('工作区恢复权限尚未准备好。');
  return transport;
}

export function useRecoveryMutation<TData, TError = Error, TVariables = void, TContext = unknown>(
  transport: ApiTransportPort,
  options: Omit<UseMutationOptions<TData, TError, TVariables, TContext>, 'mutationFn'> & {
    mutationFn(variables: TVariables, admitted: ApiTransportPort, context: MutationFunctionContext): Promise<TData>;
    /** Local intent resources only; release never commits a response or touches server cache. */
    acquireLocal?: (variables: TVariables) => () => void;
  },
): UseMutationResult<TData, TError, TVariables, TContext> {
  const { acquireLocal, ...queryOptions } = options;
  type Intent = Readonly<{ variables: TVariables; transport: ApiTransportPort }>;
  const live = (intent: Intent) => {
    try { intent.transport.recovery?.checkpoint()(); return true; } catch { return false; }
  };
  const mutation = useMutation<TData, TError, Intent, TContext>({
    ...queryOptions,
    // No paused mutation or retry is allowed to turn yesterday's intent into a new write.
    networkMode: __NC_BUNDLED__ ? 'always' : options.networkMode, retry: __NC_BUNDLED__ ? false : options.retry,
    mutationFn: (intent, context: MutationFunctionContext) => {
      intent.transport.recovery?.checkpoint()();
      return options.mutationFn(intent.variables, intent.transport, context);
    },
    onMutate: options.onMutate ? (intent, context) => {
      intent.transport.recovery?.capture();
      return options.onMutate!(intent.variables, context);
    } : undefined,
    onSuccess: (data, intent, result, context) => live(intent) ? options.onSuccess?.(data, intent.variables, result, context) : undefined,
    onError: (error, intent, result, context) => live(intent) ? options.onError?.(error, intent.variables, result, context) : undefined,
    onSettled: (data, error, intent, result, context) => live(intent) ? options.onSettled?.(data, error, intent.variables, result, context) : undefined,
  });
  const mutateAsync: UseMutationResult<TData, TError, TVariables, TContext>['mutateAsync'] = async (variables, callbacks) => {
    const admitted = admitTransport(transport);
    const release = acquireLocal?.(variables);
    try {
      const result = await mutation.mutateAsync({ variables, transport: admitted }, callbacks ? {
        onSuccess: (data, intent, result, context) => live(intent) ? callbacks.onSuccess?.(data, intent.variables, result, context) : undefined,
        onError: (error, intent, result, context) => live(intent) ? callbacks.onError?.(error, intent.variables, result, context) : undefined,
        onSettled: (data, error, intent, result, context) => live(intent) ? callbacks.onSettled?.(data, error, intent.variables, result, context) : undefined,
      } : undefined);
      admitted.recovery?.checkpoint()();
      return result;
    } finally { release?.(); }
  };
  return { ...mutation, variables: mutation.variables?.variables, mutateAsync,
    mutate: (variables, callbacks) => { void mutateAsync(variables, callbacks).catch(() => undefined); },
  } as UseMutationResult<TData, TError, TVariables, TContext>;
}
