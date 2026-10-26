import { useLayerDismissal, type UseLayerDismissalOptions, type UseLayerDismissalReturn } from '@astryxdesign/core/Layer';
import { useImperativeHandle, type Ref } from 'react';

type Props = UseLayerDismissalOptions & { ref?: Ref<UseLayerDismissalReturn> };

/** Each open lifetime gets a new Astryx identity; ordinary rerenders retain its order.
 * Mount beside (outside) the owner's LayerDepthProvider, just like the hook. */
export function LayerDismissal(props: Props) {
  return props.isActive ? <ActiveLayerDismissal {...props} /> : null;
}

function ActiveLayerDismissal({ ref, ...options }: Props) {
  const dismissal = useLayerDismissal(options);
  useImperativeHandle(ref, () => dismissal, [dismissal]);
  return null;
}
