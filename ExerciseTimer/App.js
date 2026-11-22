import { useState } from 'react';
import { StatusBar, StyleSheet } from 'react-native';
import { SafeAreaProvider, SafeAreaView } from 'react-native-safe-area-context';
import CatalogueScreen from './src/CatalogueScreen';
import TimerScreen from './src/TimerScreen';

export default function App() {
  const [selectedExercise, setSelectedExercise] = useState(null);

  return (
    <SafeAreaProvider>
      <SafeAreaView style={styles.container}>
        <StatusBar barStyle="dark-content" />
        {selectedExercise ? (
          <TimerScreen
            exercise={selectedExercise}
            onBack={() => setSelectedExercise(null)}
          />
        ) : (
          <CatalogueScreen onSelect={setSelectedExercise} />
        )}
      </SafeAreaView>
    </SafeAreaProvider>
  );
}

const styles = StyleSheet.create({
  container: {
    flex: 1,
    backgroundColor: '#f4f6f8',
  },
});
