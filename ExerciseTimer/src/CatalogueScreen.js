import { Pressable, SectionList, StyleSheet, Text, View } from 'react-native';
import { SECTIONS } from './exercises';

export default function CatalogueScreen({ onSelect }) {
  return (
    <View style={styles.container}>
      <Text style={styles.heading}>Choose an exercise</Text>
      <SectionList
        sections={SECTIONS}
        keyExtractor={(item) => item.id}
        renderSectionHeader={({ section }) => (
          <Text style={styles.sectionHeader}>{section.title}</Text>
        )}
        renderItem={({ item }) => (
          <Pressable
            style={({ pressed }) => [styles.card, pressed && styles.cardPressed]}
            onPress={() => onSelect(item)}
            accessibilityRole="button"
            accessibilityLabel={`Select ${item.name}`}
          >
            <Text style={styles.name}>{item.name}</Text>
            <Text style={styles.chevron}>›</Text>
          </Pressable>
        )}
        contentContainerStyle={styles.list}
        stickySectionHeadersEnabled={false}
      />
    </View>
  );
}

const styles = StyleSheet.create({
  container: { flex: 1, paddingHorizontal: 16 },
  heading: { fontSize: 28, fontWeight: '700', marginTop: 24, marginBottom: 8, color: '#1a1a1a' },
  list: { paddingBottom: 32 },
  sectionHeader: {
    fontSize: 14,
    fontWeight: '700',
    color: '#6b7280',
    textTransform: 'uppercase',
    marginTop: 20,
    marginBottom: 8,
  },
  card: {
    flexDirection: 'row',
    alignItems: 'center',
    justifyContent: 'space-between',
    backgroundColor: '#fff',
    borderRadius: 12,
    paddingVertical: 14,
    paddingHorizontal: 16,
    marginBottom: 8,
    shadowColor: '#000',
    shadowOpacity: 0.05,
    shadowRadius: 6,
    shadowOffset: { width: 0, height: 2 },
    elevation: 2,
  },
  cardPressed: { opacity: 0.7 },
  name: { fontSize: 17, fontWeight: '500', color: '#1a1a1a' },
  chevron: { fontSize: 22, color: '#9ca3af' },
});
