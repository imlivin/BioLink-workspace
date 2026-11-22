import { useEffect, useRef, useState } from 'react';
import { Pressable, StyleSheet, Text, View } from 'react-native';
import { recordExercise, recordStop } from './workoutService';

// Timer resolution: 50 Hz => one tick every 20 ms.
const TICK_MS = 20;

function formatTime(ms) {
  const totalSeconds = Math.floor(ms / 1000);
  const minutes = String(Math.floor(totalSeconds / 60)).padStart(2, '0');
  const seconds = String(totalSeconds % 60).padStart(2, '0');
  // Hundredths of a second (two digits) to reflect the 20 ms resolution.
  const hundredths = String(Math.floor((ms % 1000) / 10)).padStart(2, '0');
  return `${minutes}:${seconds}.${hundredths}`;
}

export default function TimerScreen({ exercise, onBack }) {
  const [isRunning, setIsRunning] = useState(false);
  const [elapsed, setElapsed] = useState(0);
  const [saveStatus, setSaveStatus] = useState(null); // 'saving' | 'saved' | 'error' | null
  // Time from earlier runs, plus the clock time the current run began.
  // Measuring against the clock keeps the timer accurate even if ticks are delayed.
  const accumulatedRef = useRef(0);
  const startedAtRef = useRef(null);

  useEffect(() => {
    if (!isRunning) return undefined;
    const interval = setInterval(() => {
      setElapsed(accumulatedRef.current + (Date.now() - startedAtRef.current));
    }, TICK_MS);
    return () => clearInterval(interval);
  }, [isRunning]);

  const start = () => {
    startedAtRef.current = Date.now();
    setIsRunning(true);
    // Record the exercise + timestamp.
    setSaveStatus('saving');
    recordExercise(exercise.dbKey)
      .then(() => setSaveStatus('saved'))
      .catch((err) => {
        console.warn('Failed to record start:', err);
        setSaveStatus('error');
      });
  };

  const stop = () => {
    accumulatedRef.current += Date.now() - startedAtRef.current;
    setElapsed(accumulatedRef.current);
    setIsRunning(false);
    // Record "none" + timestamp.
    setSaveStatus('saving');
    recordStop()
      .then(() => setSaveStatus('saved'))
      .catch((err) => {
        console.warn('Failed to record stop:', err);
        setSaveStatus('error');
      });
  };

  const reset = () => {
    accumulatedRef.current = 0;
    setElapsed(0);
    setSaveStatus(null);
  };

  const hasStopped = !isRunning && elapsed > 0;

  return (
    <View style={styles.container}>
      <Pressable
        onPress={onBack}
        disabled={isRunning}
        style={styles.back}
        accessibilityRole="button"
      >
        <Text style={[styles.backText, isRunning && styles.disabledText]}>‹ Exercises</Text>
      </Pressable>

      <View style={styles.body}>
        <Text style={styles.category}>{exercise.category}</Text>
        <Text style={styles.name}>{exercise.name}</Text>

        <Text style={styles.time}>{formatTime(elapsed)}</Text>
        <View style={styles.statusBlock}>
          <Text style={styles.status}>
            {isRunning ? 'In progress' : hasStopped ? 'Stopped' : 'Ready'}
          </Text>
          {saveStatus && (
            <Text style={styles.saveStatus}>
              {saveStatus === 'saving'
                ? 'Saving…'
                : saveStatus === 'saved'
                  ? 'Saved to cloud'
                  : 'Save failed'}
            </Text>
          )}
        </View>

        <Pressable
          style={({ pressed }) => [
            styles.button,
            isRunning ? styles.stopButton : styles.startButton,
            pressed && styles.pressed,
          ]}
          onPress={isRunning ? stop : start}
          accessibilityRole="button"
        >
          <Text style={styles.buttonText}>
            {isRunning ? 'Stop' : hasStopped ? 'Resume' : 'Start'}
          </Text>
        </Pressable>

        {hasStopped && (
          <Pressable onPress={reset} style={styles.resetButton} accessibilityRole="button">
            <Text style={styles.resetText}>Reset</Text>
          </Pressable>
        )}
      </View>
    </View>
  );
}

const styles = StyleSheet.create({
  container: { flex: 1, paddingHorizontal: 16 },
  back: { paddingVertical: 16, alignSelf: 'flex-start' },
  backText: { fontSize: 17, color: '#2563eb' },
  disabledText: { color: '#9ca3af' },
  body: { flex: 1, alignItems: 'center', justifyContent: 'center', paddingBottom: 60 },
  category: { fontSize: 14, fontWeight: '700', color: '#6b7280', textTransform: 'uppercase' },
  name: { fontSize: 32, fontWeight: '700', color: '#1a1a1a', marginTop: 4, textAlign: 'center' },
  time: { fontSize: 64, fontWeight: '300', color: '#1a1a1a', marginTop: 48, fontVariant: ['tabular-nums'] },
  statusBlock: { height: 72, alignItems: 'center', justifyContent: 'flex-start', marginBottom: 16 },
  status: { fontSize: 16, color: '#6b7280' },
  saveStatus: { fontSize: 14, color: '#6b7280', marginTop: 4 },
  button: {
    width: 180,
    height: 180,
    borderRadius: 90,
    alignItems: 'center',
    justifyContent: 'center',
  },
  startButton: { backgroundColor: '#16a34a' },
  stopButton: { backgroundColor: '#dc2626' },
  pressed: { opacity: 0.8 },
  buttonText: { color: '#fff', fontSize: 28, fontWeight: '700' },
  resetButton: { marginTop: 24, padding: 12 },
  resetText: { fontSize: 17, color: '#2563eb' },
});
