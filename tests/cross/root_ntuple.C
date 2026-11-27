// Writes, with ROOT itself, an RNTuple with one field of every type the
// writer supports: bool, (unsigned) 8/16/32/64-bit integers, float,
// double, std::array<float, 3>, std::vector<double>, std::string: the
// values of root_tree.C. The file is a fixture (zlib level 1, as rhizoid
// writes by default).
//
//   root -b -q -l 'tests/cross/root_ntuple.C("root_ntuple.root")'
//
// With a second argument, reads the RNTuple of that name instead, with
// ROOT's checks on (page checksums), and prints what it holds: the entry
// count, the clusters, each field's type, then every entry, field by
// field, in root_tree.C's format; and the inspector's column summary.
//
//   root -b -q -l 'tests/cross/root_ntuple.C("file.root", "t")'

#include <ROOT/RNTupleInspector.hxx>
#include <ROOT/RNTupleModel.hxx>
#include <ROOT/RNTupleReader.hxx>
#include <ROOT/RNTupleWriteOptions.hxx>
#include <ROOT/RNTupleWriter.hxx>
#include <TFile.h>
#include <TROOT.h>
#include <TSystem.h>

#include <algorithm>
#include <array>
#include <cstdint>
#include <cstdio>
#include <string>
#include <utility>
#include <vector>

// ROOT 6.34 has the RNTuple classes in ROOT::Experimental; later versions
// in ROOT.
#if ROOT_VERSION_CODE < ROOT_VERSION(6, 36, 0)
namespace RNT = ROOT::Experimental;
#else
namespace RNT = ROOT;
#endif

// A descriptor, returned by reference or (ROOT 6.34's inspector) by pointer.
template <typename T>
static const T &deref(const T &d)
{
   return d;
}
template <typename T>
static const T &deref(const T *d)
{
   return *d;
}

template <typename T>
static void put(Long64_t i, const char *name, T v)
{
   printf("%lld %s %.17g\n", i, name, static_cast<double>(v));
}

static void dump(const char *path, const char *name)
{
   setvbuf(stdout, nullptr, _IOLBF, 0);
   auto reader = RNT::RNTupleReader::Open(name, path);
   const auto &desc = deref(reader->GetDescriptor());
   printf("NTUPLE entries %llu\n", static_cast<unsigned long long>(reader->GetNEntries()));
   for (const auto &f : desc.GetTopLevelFields())
      printf("FIELD %s %s\n", f.GetFieldName().c_str(), f.GetTypeName().c_str());
   auto b = reader->GetView<bool>("b");
   auto i8 = reader->GetView<std::int8_t>("i8");
   auto u8 = reader->GetView<std::uint8_t>("u8");
   auto i16 = reader->GetView<std::int16_t>("i16");
   auto u16 = reader->GetView<std::uint16_t>("u16");
   auto i32 = reader->GetView<std::int32_t>("i32");
   auto u32 = reader->GetView<std::uint32_t>("u32");
   auto i64 = reader->GetView<std::int64_t>("i64");
   auto u64 = reader->GetView<std::uint64_t>("u64");
   auto f32 = reader->GetView<float>("f32");
   auto f64 = reader->GetView<double>("f64");
   auto xyz = reader->GetView<std::array<float, 3>>("xyz");
   auto jag = reader->GetView<std::vector<double>>("jag");
   auto str = reader->GetView<std::string>("str");
   for (auto i : reader->GetEntryRange()) {
      Long64_t e = i;
      put(e, "b", b(i));
      put(e, "i8", i8(i));
      put(e, "u8", u8(i));
      put(e, "i16", i16(i));
      put(e, "u16", u16(i));
      put(e, "i32", i32(i));
      put(e, "u32", u32(i));
      printf("%lld i64 %lld\n", e, static_cast<long long>(i64(i)));
      printf("%lld u64 %llu\n", e, static_cast<unsigned long long>(u64(i)));
      put(e, "f32", f32(i));
      put(e, "f64", f64(i));
      printf("%lld xyz", e);
      for (float v : xyz(i))
         printf(" %.17g", v);
      printf("\n%lld jag", e);
      for (double v : jag(i))
         printf(" %.17g", v);
      printf("\n%lld str %s\n", e, str(i).c_str());
   }
   // The inspector walks every column and page of the descriptor.
   auto inspector = ROOT::Experimental::RNTupleInspector::Create(name, path);
   const auto &full = deref(inspector->GetDescriptor());
   // Column 0 (field `b`) has one element per entry.
   for (std::uint64_t entry = 0; entry < reader->GetNEntries();) {
      const auto &c = full.GetClusterDescriptor(full.FindClusterId(0, entry));
      entry = c.GetFirstEntryIndex() + c.GetNEntries();
      printf("CLUSTER %llu %llu\n", static_cast<unsigned long long>(c.GetFirstEntryIndex()),
             static_cast<unsigned long long>(entry));
   }

   printf("INSPECT columns %zu entries %llu\n", full.GetNPhysicalColumns(),
          static_cast<unsigned long long>(full.GetNEntries()));
}

void root_ntuple(const char *path, const char *dump_name = nullptr)
{
   if (dump_name) {
      dump(path, dump_name);
      return;
   }
   auto model = RNT::RNTupleModel::Create();
   auto b = model->MakeField<bool>("b");
   auto i8 = model->MakeField<std::int8_t>("i8");
   auto u8 = model->MakeField<std::uint8_t>("u8");
   auto i16 = model->MakeField<std::int16_t>("i16");
   auto u16 = model->MakeField<std::uint16_t>("u16");
   auto i32 = model->MakeField<std::int32_t>("i32");
   auto u32 = model->MakeField<std::uint32_t>("u32");
   auto i64 = model->MakeField<std::int64_t>("i64");
   auto u64 = model->MakeField<std::uint64_t>("u64");
   auto f32 = model->MakeField<float>("f32");
   auto f64 = model->MakeField<double>("f64");
   auto xyz = model->MakeField<std::array<float, 3>>("xyz");
   auto jag = model->MakeField<std::vector<double>>("jag");
   auto str = model->MakeField<std::string>("str");
   RNT::RNTupleWriteOptions options;
   options.SetCompression(101);
   {
      auto file = std::unique_ptr<TFile>(TFile::Open(path, "RECREATE"));
      auto writer = RNT::RNTupleWriter::Append(std::move(model), "t", *file, options);
      for (int i = 0; i < 20; ++i) {
         *b = i % 3 == 0;
         *i8 = -i;
         *u8 = 200 + i;
         *i16 = -1000 * i;
         *u16 = 60000 + i;
         *i32 = -100000 * i;
         *u32 = 4000000000u + i;
         *i64 = -10000000000LL * i;
         *u64 = 18000000000000000000ULL + i;
         *f32 = 0.5f * i;
         *f64 = 0.25 * i;
         for (int j = 0; j < 3; ++j)
            (*xyz)[j] = i + 0.125f * j;
         jag->clear();
         for (int j = 0; j < i % 5; ++j)
            jag->push_back(i * 10 + j);
         *str = "entry " + std::to_string(i) + (i % 4 == 0 ? " is a longer string" : "");
         writer->Fill();
      }
   }
}
